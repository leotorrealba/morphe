//! User-defined function evaluation, recursion limits and tail calls.
//!
//! Every rule here is transcribed from upstream openscad
//! src/core/Expression.cc (Range/TernaryOp/MemberLookup/Let/FunctionCall,
//! including the tail-call loop and its 1,000,000 counter), Arguments.cc,
//! Parameters.cc, LocalScope.cc and parser.y (master, fetched 2026-07-02) —
//! except the non-tail call cap itself, [`MAX_CALL_DEPTH`], which is
//! morphe's own limit (upstream guards recursion with a native stack probe
//! instead).

use morphe_eval::eval::{Evaluator, MAX_CALL_DEPTH, MAX_EXPR_DEPTH, MAX_TAIL_CALLS};
use morphe_eval::value::{Range, Value};
use morphe_lang::parser::{parse_expr, parse_program};

fn n(x: f64) -> Value {
    Value::Number(x)
}

/// Evaluate a single expression in a fresh evaluator.
fn eval_source_expr(src: &str) -> Value {
    eval_expr_with(src).0
}

/// Evaluate a single expression, returning the evaluator for its sinks.
fn eval_expr_with(src: &str) -> (Value, Evaluator) {
    let mut evaluator = Evaluator::new();
    let expr = parse_expr(src).expect("expression should parse");
    let value = evaluator.eval_expr(&expr);
    (value, evaluator)
}

/// Parse and evaluate a whole program, returning the evaluator.
fn eval_program(src: &str) -> Evaluator {
    let mut evaluator = Evaluator::new();
    let program = parse_program(src).expect("program should parse");
    evaluator.eval_program(&program);
    evaluator
}

fn has_warning(evaluator: &Evaluator, needle: &str) -> bool {
    evaluator.warnings().iter().any(|w| w.contains(needle))
}

// ---------------------------------------------------------------- literals

#[test]
fn literals_evaluate() {
    assert_eq!(eval_source_expr("42"), n(42.0));
    assert_eq!(eval_source_expr("\"hi\""), Value::String("hi".into()));
    assert_eq!(eval_source_expr("true"), Value::Bool(true));
    assert_eq!(eval_source_expr("false"), Value::Bool(false));
    assert_eq!(eval_source_expr("undef"), Value::Undef(None));
}

#[test]
fn vector_literal_evaluates_elements() {
    assert_eq!(
        eval_source_expr("[1, 1 + 1, [3]]"),
        Value::from(vec![n(1.0), n(2.0), Value::from(vec![n(3.0)])])
    );
}

#[test]
fn range_literal_evaluates() {
    assert_eq!(
        eval_source_expr("[0 : 2 : 10]"),
        Value::Range(Range::new(0.0, 2.0, 10.0))
    );
    // Upstream Range::evaluate defaults a missing step to 1.0.
    assert_eq!(
        eval_source_expr("[1 : 5]"),
        Value::Range(Range::new(1.0, 1.0, 5.0))
    );
}

#[test]
fn range_non_number_endpoint_warns_and_yields_undef() {
    // Upstream Expression.cc Range::evaluate: a begin/end that fails
    // getDouble() warns "Unable to convert [...] to a range" and returns
    // plain undefined.
    let (value, evaluator) = eval_expr_with("[\"a\" : 5]");
    assert_eq!(value, Value::Undef(None));
    assert!(has_warning(&evaluator, "Unable to convert"));
    assert!(has_warning(&evaluator, "to a range"));
}

#[test]
fn range_non_number_step_warns_and_yields_undef() {
    let (value, evaluator) = eval_expr_with("[0 : \"s\" : 5]");
    assert_eq!(value, Value::Undef(None));
    assert!(has_warning(&evaluator, "to a step value"));
}

// ----------------------------------------------------------------- lookups

#[test]
fn unknown_ident_warns_and_yields_undef() {
    let (value, evaluator) = eval_expr_with("nope");
    assert_eq!(value, Value::Undef(None));
    assert!(has_warning(&evaluator, "Ignoring unknown variable 'nope'"));
}

#[test]
fn builtin_pi_is_visible() {
    assert_eq!(eval_source_expr("PI"), n(std::f64::consts::PI));
}

// --------------------------------------------------------------- operators

#[test]
fn arithmetic_operators_evaluate() {
    assert_eq!(eval_source_expr("1 + 2 * 3"), n(7.0));
    assert_eq!(eval_source_expr("2 ^ 10"), n(1024.0));
    assert_eq!(eval_source_expr("7 % 4"), n(3.0));
    assert_eq!(eval_source_expr("-(3)"), n(-3.0));
    // parser.y: unary '+' is the identity.
    assert_eq!(eval_source_expr("+5"), n(5.0));
    assert_eq!(eval_source_expr("![]"), Value::Bool(true));
}

#[test]
fn vector_arithmetic_evaluates() {
    assert_eq!(
        eval_source_expr("[1, 2] + [3, 4]"),
        Value::from(vec![n(4.0), n(6.0)])
    );
    // Dot product via the Value multiply visitor.
    assert_eq!(eval_source_expr("[1, 2] * [3, 4]"), n(11.0));
}

#[test]
fn undef_operation_warns_with_upstream_reason() {
    // Upstream Expression::checkUndef logs the undef reason as a warning.
    let (value, evaluator) = eval_expr_with("1 + \"a\"");
    assert!(value.is_undef());
    assert!(has_warning(
        &evaluator,
        "undefined operation (number + string)"
    ));

    let (value, evaluator) = eval_expr_with("-\"a\"");
    assert!(value.is_undef());
    assert!(has_warning(&evaluator, "undefined operation (-string)"));
}

#[test]
fn logical_operators_short_circuit() {
    // Upstream BinaryOp::evaluate uses C++ && / || over toBool(), so the
    // untaken side is never evaluated: no unknown-variable warning.
    let (value, evaluator) = eval_expr_with("false && nope");
    assert_eq!(value, Value::Bool(false));
    assert!(evaluator.warnings().is_empty());

    let (value, evaluator) = eval_expr_with("true || nope");
    assert_eq!(value, Value::Bool(true));
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn equality_compares_values() {
    assert_eq!(eval_source_expr("1 == 1"), Value::Bool(true));
    assert_eq!(eval_source_expr("1 != 2"), Value::Bool(true));
    // Cross-type comparison is false, not undef (equals_visitor).
    assert_eq!(eval_source_expr("1 == \"1\""), Value::Bool(false));
}

#[test]
fn ternary_evaluates_only_taken_branch() {
    // Upstream TernaryOp::evaluateStep picks the branch before evaluating.
    let (value, evaluator) = eval_expr_with("true ? 1 : nope");
    assert_eq!(value, n(1.0));
    assert!(evaluator.warnings().is_empty());

    let (value, evaluator) = eval_expr_with("false ? nope : 2");
    assert_eq!(value, n(2.0));
    assert!(evaluator.warnings().is_empty());

    // Condition coerces via toBool: empty string is falsy.
    assert_eq!(eval_source_expr("\"\" ? 1 : 2"), n(2.0));
}

// ------------------------------------------------------------ index/member

#[test]
fn index_evaluates() {
    assert_eq!(eval_source_expr("[10, 20, 30][1]"), n(20.0));
    assert_eq!(eval_source_expr("\"abc\"[1]"), Value::String("b".into()));
}

#[test]
fn member_access_on_vectors() {
    // Upstream MemberLookup: .x/.y/.z index 0/1/2 on vectors.
    assert_eq!(eval_source_expr("[1, 2, 3].x"), n(1.0));
    assert_eq!(eval_source_expr("[1, 2, 3].y"), n(2.0));
    assert_eq!(eval_source_expr("[1, 2, 3].z"), n(3.0));
    // Other members yield plain undef without a warning.
    let (value, evaluator) = eval_expr_with("[1, 2, 3].w");
    assert_eq!(value, Value::Undef(None));
    assert!(evaluator.warnings().is_empty());
    assert_eq!(eval_source_expr("(5).x"), Value::Undef(None));
}

#[test]
fn member_access_on_ranges() {
    // Upstream MemberLookup: .begin/.step/.end on ranges.
    assert_eq!(eval_source_expr("[0 : 2 : 10].begin"), n(0.0));
    assert_eq!(eval_source_expr("[0 : 2 : 10].step"), n(2.0));
    assert_eq!(eval_source_expr("[0 : 2 : 10].end"), n(10.0));
}

// ---------------------------------------------------------------------- let

#[test]
fn let_binds_sequentially() {
    // Upstream Let::doSequentialAssignment: each binding sees the previous.
    assert_eq!(eval_source_expr("let(a = 1, b = a + 1) b"), n(2.0));
}

#[test]
fn let_scopes_do_not_leak() {
    let evaluator = eval_program("a = 1; x = let(a = 2) a; y = a;");
    let mut evaluator = evaluator;
    assert_eq!(evaluator.lookup("x"), n(2.0));
    assert_eq!(evaluator.lookup("y"), n(1.0));
}

#[test]
fn let_duplicate_binding_is_ignored_first_wins() {
    // Unlike scope bodies (last-wins), Let ignores duplicates with a
    // warning (Let::doSequentialAssignment `seen` set).
    let (value, evaluator) = eval_expr_with("let(a = 1, a = 2) a");
    assert_eq!(value, n(1.0));
    assert!(has_warning(
        &evaluator,
        "Ignoring duplicate variable assignment"
    ));
}

#[test]
fn let_positional_binding_warns() {
    let (value, evaluator) = eval_expr_with("let(5) 1");
    assert_eq!(value, n(1.0));
    assert!(has_warning(&evaluator, "Assignment without variable name"));
}

// ------------------------------------------------------------------- calls

#[test]
fn simple_function_call() {
    let mut evaluator = eval_program("function inc(x) = x + 1; y = inc(41);");
    assert_eq!(evaluator.lookup("y"), n(42.0));
}

#[test]
fn named_arguments_match_parameter_names() {
    let mut evaluator = eval_program("function f(a, b) = a - b; y = f(b = 1, a = 10);");
    assert_eq!(evaluator.lookup("y"), n(9.0));
}

#[test]
fn named_argument_overrides_positional_with_warning() {
    let mut evaluator = eval_program("function f(a) = a; y = f(1, a = 2);");
    assert_eq!(evaluator.lookup("y"), n(2.0));
    assert!(has_warning(&evaluator, "overrides positional argument"));
}

#[test]
fn default_used_when_argument_missing() {
    let mut evaluator = eval_program("function f(a = 5) = a; y = f();");
    assert_eq!(evaluator.lookup("y"), n(5.0));
}

#[test]
fn default_referencing_earlier_param_sees_definition_scope() {
    // Upstream Parameters.cc evaluates defaults in the *defining context*
    // only: earlier parameters are NOT visible to later defaults, so
    // `b = a` resolves `a` at the definition site, not to the supplied
    // argument.
    let mut evaluator = eval_program("a = 100; function f(a, b = a) = b; y = f(1);");
    assert_eq!(evaluator.lookup("y"), n(100.0));

    // With no definition-scope binding the default is undef and warns.
    let mut evaluator = eval_program("function g(a, b = a) = b; z = g(1);");
    assert_eq!(evaluator.lookup("z"), Value::Undef(None));
    assert!(has_warning(&evaluator, "Ignoring unknown variable 'a'"));
}

#[test]
fn extra_positional_argument_warns_and_is_ignored() {
    let mut evaluator = eval_program("function f(a) = a; y = f(1, 2);");
    assert_eq!(evaluator.lookup("y"), n(1.0));
    assert!(has_warning(
        &evaluator,
        "Too many unnamed arguments supplied"
    ));
}

#[test]
fn extra_named_argument_warns_but_still_binds() {
    // Upstream parse_without_defaults warns "variable ... not specified as
    // parameter" but still sets the variable in the parameter frame, so the
    // body can see it.
    let mut evaluator = eval_program("function f(a) = b; y = f(1, b = 7);");
    assert_eq!(evaluator.lookup("y"), n(7.0));
    assert!(has_warning(&evaluator, "not specified as parameter"));
}

#[test]
fn duplicate_named_argument_warns_last_wins() {
    let mut evaluator = eval_program("function f(a) = a; y = f(a = 1, a = 2);");
    assert_eq!(evaluator.lookup("y"), n(2.0));
    assert!(has_warning(&evaluator, "supplied more than once"));
}

#[test]
fn missing_argument_binds_undef_without_outer_fallthrough() {
    // Upstream binds Value::undefined for a missing parameter, so the body
    // does NOT fall through to a same-named outer variable.
    let mut evaluator = eval_program("a = 5; function f(a) = a; y = f();");
    assert_eq!(evaluator.lookup("y"), Value::Undef(None));
}

#[test]
fn arguments_evaluate_in_caller_scope() {
    // Upstream Arguments.cc evaluates argument expressions in the calling
    // context before parameters are bound.
    let mut evaluator = eval_program("x = 3; function f(a) = a; y = let(x = 5) f(x);");
    assert_eq!(evaluator.lookup("y"), n(5.0));
}

#[test]
fn function_body_sees_definition_scope_not_call_site() {
    // Lexical capture: the body context chains to the defining context.
    let mut evaluator = eval_program("x = 1; function f() = x; y = let(x = 2) f();");
    assert_eq!(evaluator.lookup("y"), n(1.0));
}

#[test]
fn dollar_arguments_propagate_dynamically_through_calls() {
    // body_context->apply_config_variables(caller): $-variables flow along
    // the dynamic call chain, through intermediate calls.
    let mut evaluator =
        eval_program("function g() = $fn; function f() = g(); y = f($fn = 7); z = g();");
    assert_eq!(evaluator.lookup("y"), n(7.0));
    // Without the $-argument, g() reads the root $fn seed (0), not undef.
    assert_eq!(evaluator.lookup("z"), n(0.0));
    assert!(!has_warning(&evaluator, "Ignoring unknown variable '$fn'"));
}

#[test]
fn unknown_function_warns_and_yields_undef() {
    // Upstream Context::lookup_function: "Ignoring unknown function ...".
    let mut evaluator = eval_program("y = nofunc(1);");
    assert_eq!(evaluator.lookup("y"), Value::Undef(None));
    assert!(has_warning(
        &evaluator,
        "Ignoring unknown function 'nofunc'"
    ));
}

#[test]
fn non_identifier_callee_warns_and_yields_undef() {
    // Upstream FunctionCall::evaluate_function_expression on a non-lookup
    // callee that is not a function value: "Can't call function on ...".
    let (value, evaluator) = eval_expr_with("(\"s\")(1)");
    assert_eq!(value, Value::Undef(None));
    assert!(has_warning(&evaluator, "Can't call function on string"));
}

// --------------------------------------------------------------- recursion

#[test]
fn recursion_fib_10() {
    let mut evaluator =
        eval_program("function fib(n) = n < 2 ? n : fib(n - 1) + fib(n - 2); y = fib(10);");
    assert_eq!(evaluator.lookup("y"), n(55.0));
    assert!(evaluator.errors().is_empty());
}

#[test]
fn mutual_recursion() {
    let mut evaluator = eval_program(
        "function is_even(n) = n == 0 ? true : is_odd(n - 1); \
         function is_odd(n) = n == 0 ? false : is_even(n - 1); \
         y = is_even(10);",
    );
    assert_eq!(evaluator.lookup("y"), Value::Bool(true));
    assert!(evaluator.errors().is_empty());
}

#[test]
fn recursion_up_to_the_limit_succeeds() {
    // Non-tail recursion: down(MAX_CALL_DEPTH - 1) nests exactly
    // MAX_CALL_DEPTH calls.
    let depth = MAX_CALL_DEPTH - 1;
    let mut evaluator = eval_program(&format!(
        "function down(n) = n == 0 ? 0 : 1 + down(n - 1); y = down({depth});"
    ));
    assert!(evaluator.errors().is_empty(), "{:?}", evaluator.errors());
    assert_eq!(evaluator.lookup("y"), n(depth as f64));
}

#[test]
fn non_tail_recursion_two_thousand_deep() {
    let mut evaluator = eval_program(
        "function sum(n) = n == 0 ? 0 : n + sum(n - 1); y = sum(2000); \
         function build(n) = n == 0 ? [] : concat(build(n - 1), [n]); z = len(build(2000));",
    );
    assert!(evaluator.errors().is_empty(), "{:?}", evaluator.errors());
    assert_eq!(evaluator.lookup("y"), n(2001000.0));
    assert_eq!(evaluator.lookup("z"), n(2000.0));
}

#[test]
fn recursion_limit_errors_with_span() {
    // Exceeding the non-tail call cap records an error carrying the span of
    // the recursive call expression, and evaluation short-circuits to undef
    // instead of crashing.
    let src = "function f(n) = 1 + f(n + 1); y = f(0);";
    let mut evaluator = eval_program(src);
    assert_eq!(evaluator.lookup("y"), Value::Undef(None));
    assert_eq!(evaluator.errors().len(), 1);
    let error = &evaluator.errors()[0];
    assert!(
        error.message.contains("recursion limit exceeded"),
        "unexpected message: {}",
        error.message
    );
    assert!(error.message.contains("'f'"));
    // The span points at the recursive call inside the function body.
    assert_eq!(&src[error.span.start..error.span.end], "f(n + 1)");
}

// -------------------------------------------------------------- tail calls

#[test]
fn tail_recursion_one_thousand_deep() {
    let mut evaluator =
        eval_program("function f(n, acc = 0) = n == 0 ? acc : f(n - 1, acc + 1); y = f(1000);");
    assert!(evaluator.errors().is_empty(), "{:?}", evaluator.errors());
    assert_eq!(evaluator.lookup("y"), n(1000.0));
}

#[test]
fn tail_recursion_one_hundred_thousand_deep() {
    let mut evaluator =
        eval_program("function f(n, acc = 0) = n == 0 ? acc : f(n - 1, acc + 1); y = f(100000);");
    assert!(evaluator.errors().is_empty(), "{:?}", evaluator.errors());
    assert_eq!(evaluator.lookup("y"), n(100000.0));
}

#[test]
fn tail_call_through_let_and_nested_ternaries() {
    // The tail position is found after peeling `let`s and ternaries in any
    // arrangement (upstream simplify_function_body).
    let mut evaluator = eval_program(
        "function f(n, acc = []) = let(m = n - 1) n <= 0 ? acc \
             : n % 2 == 0 ? let(e = n) f(m, concat(acc, [e])) : f(m, acc); \
         y = len(f(5000)); z = f(6);",
    );
    assert!(evaluator.errors().is_empty(), "{:?}", evaluator.errors());
    assert_eq!(evaluator.lookup("y"), n(2500.0));
    assert_eq!(evaluator.lookup("z").to_string(), "[6, 4, 2]");
}

#[test]
fn mutual_tail_recursion_runs_deep() {
    // Upstream eliminates any user-function call in tail position, not only
    // self-calls.
    let mut evaluator = eval_program(
        "function is_even(n) = n == 0 ? true : is_odd(n - 1); \
         function is_odd(n) = n == 0 ? false : is_even(n - 1); \
         y = is_even(100001);",
    );
    assert!(evaluator.errors().is_empty(), "{:?}", evaluator.errors());
    assert_eq!(evaluator.lookup("y"), Value::Bool(false));
}

#[test]
fn tail_calls_keep_dollar_variables_flowing() {
    // `$`-arguments of one tail call are visible to the next, and the
    // caller's `$`-variables stay visible throughout.
    let mut evaluator = eval_program(
        "function g(n) = n <= 0 ? $x : g(n - 1, $x = n); y = g(5, $x = 99); \
         function h(n) = n == 0 ? $fn : h(n - 1); $fn = 7; z = h(10); \
         function k(n) = n == 0 ? $y : let($y = n) k(n - 1); w = k(3);",
    );
    assert!(evaluator.errors().is_empty(), "{:?}", evaluator.errors());
    assert_eq!(evaluator.lookup("y"), n(1.0));
    assert_eq!(evaluator.lookup("z"), n(7.0));
    assert_eq!(evaluator.lookup("w"), n(1.0));
}

#[test]
fn infinite_tail_recursion_is_detected() {
    // Upstream FunctionCall::evaluate: the 1,000,001st chained call reports
    // "Recursion detected calling function". f(999999) chains exactly
    // MAX_TAIL_CALLS calls and still succeeds.
    assert_eq!(MAX_TAIL_CALLS, 1_000_000);
    let src = "function f(n) = n == 0 ? 0 : f(n - 1); ok = f(999999); \
               function inf(n) = inf(n + 1); y = inf(0); after = 1;";
    let mut evaluator = eval_program(src);
    assert_eq!(evaluator.lookup("ok"), n(0.0));
    assert_eq!(evaluator.lookup("y"), Value::Undef(None));
    assert_eq!(evaluator.lookup("after"), n(1.0));
    assert_eq!(evaluator.errors().len(), 1, "{:?}", evaluator.errors());
    let error = &evaluator.errors()[0];
    assert_eq!(error.message, "Recursion detected calling function 'inf'");
    assert_eq!(&src[error.span.start..error.span.end], "inf(n + 1)");
}

#[test]
fn deeply_nested_tail_built_value_drops_without_overflow() {
    // A tail-recursive accumulator can nest a value a million levels deep.
    // Releasing it must not recurse on the native stack: it is dropped here
    // on the test harness thread, far smaller than the nesting would need.
    let mut evaluator =
        eval_program("function f(n, acc = []) = n == 0 ? acc : f(n - 1, [acc]); x = f(999999);");
    assert!(evaluator.errors().is_empty(), "{:?}", evaluator.errors());
    let mut depth = 0;
    let mut value = evaluator.lookup("x");
    while let Value::Vector(items) = &value {
        let Some(first) = items.first().cloned() else {
            break;
        };
        value = first;
        depth += 1;
    }
    assert_eq!(depth, 999999);
    drop(value);
    drop(evaluator);
}

// ----------------------------------------------------------- eval_program

#[test]
fn assignment_last_wins_at_first_position() {
    // parser.y handle_assignment: the merged program body evaluates the
    // final expression at the first assignment's position.
    let mut evaluator = eval_program("x = 1; y = x; x = 2;");
    assert_eq!(evaluator.lookup("x"), n(2.0));
    assert_eq!(evaluator.lookup("y"), n(2.0));
    assert!(has_warning(&evaluator, "was assigned twice"));
}

#[test]
fn functions_are_visible_before_their_definition() {
    // Upstream stores definitions in the scope's LocalScope maps, so a
    // call textually before the definition still resolves.
    let mut evaluator = eval_program("y = f(); function f() = 3;");
    assert_eq!(evaluator.lookup("y"), n(3.0));
}

#[test]
fn function_redefinition_silently_replaces() {
    // LocalScope::addFunction insert-or-assigns without a warning.
    let mut evaluator = eval_program("function f() = 1; function f() = 2; y = f();");
    assert_eq!(evaluator.lookup("y"), n(2.0));
    assert!(!has_warning(&evaluator, "f"));
}

#[test]
fn module_definitions_are_stored_without_evaluation() {
    let mut evaluator = eval_program("module m() { } x = 1;");
    assert_eq!(evaluator.lookup("x"), n(1.0));
    assert!(evaluator.warnings().is_empty());
    assert!(evaluator.errors().is_empty());
}

#[test]
fn statements_evaluate_without_notes() {
    // Statement evaluation is covered in tests/modules.rs; plain builtin
    // instantiations leave no warning note.
    let evaluator = eval_program("cube(1);");
    assert!(evaluator.warnings().is_empty());
}

// ---------------------------------------------------------- comprehensions

#[test]
fn comprehension_elements_evaluate() {
    // Comprehension evaluation is covered in tests/comprehensions.rs: no
    // stub warning, and the elements flatten into the enclosing vector.
    let (value, evaluator) = eval_expr_with("[for (i = [0 : 3]) i]");
    assert_eq!(
        value,
        Value::from(vec![
            Value::Number(0.0),
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(3.0),
        ])
    );
    assert!(evaluator.warnings().is_empty());
}

// -------------------------------------------- $-visibility during defaults
//
// Upstream ground truth (Parameters.cc + empirical OpenSCAD 2021.01 runs):
// defaults evaluate before the Parameters object's ContextFrameHandle puts
// the argument frame on the special-variable stack, so a default sees
// `$`-variables bound along the caller's dynamic chain, but NOT
// `$`-arguments supplied in the same call.

#[test]
fn default_sees_caller_dollar_binding_dynamically() {
    // OpenSCAD 2021.01: `$fn = 12; function f(a = $fn) = a; f()` echoes 12.
    let mut evaluator = eval_program("$fn = 12; function f(a = $fn) = a; y = f();");
    assert_eq!(evaluator.lookup("y"), n(12.0));

    // Same through a let-bound special variable at the call site:
    // `function q(v = $x) = v; let($x = 5) q()` echoes 5.
    let mut evaluator = eval_program("function q(v = $x) = v; y = let($x = 5) q();");
    assert_eq!(evaluator.lookup("y"), n(5.0));
}

#[test]
fn default_does_not_see_same_call_dollar_argument() {
    // OpenSCAD 2021.01: `function f(a = $fn) = a; f($fn = 12)` echoes 0 —
    // the root $fn seed, not the argument (the default evaluates in the
    // defining context before the call's $-argument reaches the
    // special-variable stack).
    let mut evaluator = eval_program("function f(a = $fn) = a; y = f($fn = 12);");
    assert_eq!(evaluator.lookup("y"), n(0.0));
    assert!(!has_warning(&evaluator, "Ignoring unknown variable '$fn'"));
}

#[test]
fn default_does_not_see_earlier_dollar_param() {
    // OpenSCAD 2021.01: `function g($a, b = $a) = b; g(3)` echoes undef —
    // the positionally-supplied $a is not on the special-variable stack
    // while defaults evaluate...
    let mut evaluator = eval_program("function g($a, b = $a) = b; z = g(3);");
    assert_eq!(evaluator.lookup("z"), Value::Undef(None));
    assert!(has_warning(&evaluator, "Ignoring unknown variable '$a'"));

    // ...but it IS bound for the body: `function p($a) = $a; p(3)` echoes 3.
    let mut evaluator = eval_program("function p($a) = $a; w = p(3);");
    assert_eq!(evaluator.lookup("w"), n(3.0));
}

// -------------------------------------------------- expression depth guard

/// A right-leaning chain of `depth` unary negations around `1`, built
/// programmatically: the parser caps its own nesting, but the evaluator
/// must survive any AST.
fn nested_neg(depth: usize) -> morphe_lang::ast::Expr {
    use morphe_lang::ast::{Expr, ExprKind, UnaryOp};
    use morphe_lang::lexer::Span;

    let span = Span { start: 0, end: 1 };
    let mut expr = Expr {
        kind: ExprKind::Number(1.0),
        span,
    };
    for _ in 0..depth {
        expr = Expr {
            kind: ExprKind::Unary {
                op: UnaryOp::Neg,
                operand: Box::new(expr),
            },
            span,
        };
    }
    expr
}

#[test]
fn expression_nesting_below_the_limit_evaluates() {
    let mut evaluator = Evaluator::new();
    assert_eq!(evaluator.eval_expr(&nested_neg(1000)), n(1.0));
    assert!(evaluator.errors().is_empty());
}

#[test]
fn expression_nesting_too_deep_errors_with_span() {
    // A dedicated big stack makes the native cost of MAX_EXPR_DEPTH
    // wrapper frames irrelevant to the harness thread size: the test
    // must observe the guard, never a real overflow.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(expression_nesting_too_deep_body)
        .expect("spawn")
        .join()
        .expect("join");
}

fn expression_nesting_too_deep_body() {
    use morphe_lang::lexer::Span;

    let mut evaluator = Evaluator::new();
    let value = evaluator.eval_expr(&nested_neg(MAX_EXPR_DEPTH + 100));
    assert!(value.is_undef());
    assert_eq!(evaluator.errors().len(), 1);
    let error = &evaluator.errors()[0];
    assert!(
        error.message.contains("expression nesting too deep"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.span, Span { start: 0, end: 1 });

    // The depth counter unwinds: the evaluator stays usable.
    let expr = parse_expr("1 + 1").expect("parse");
    assert_eq!(evaluator.eval_expr(&expr), n(2.0));
}
