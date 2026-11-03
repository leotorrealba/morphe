//! Function literals and function values.
//!
//! Upstream: `src/core/Expression.cc` (`FunctionDefinition::evaluate`,
//! `FunctionCall::evaluate_function_expression`, `simplify_function_body`),
//! `src/core/Value.h` / `Value.cc` (`FunctionPtr`, `FunctionType`, the
//! `operator<<` printing a function) and `src/core/Context.cc` /
//! `ContextFrame.cc` (`lookup_function` / `lookup_local_function`).

use morphe_eval::eval::Evaluator;
use morphe_lang::parser::parse_program;

/// Evaluate `src` and return the evaluator (console, warnings, errors).
fn run(src: &str) -> Evaluator {
    let mut ev = Evaluator::new();
    ev.set_source(src);
    let program = parse_program(src).expect("program parses");
    ev.eval_program(&program);
    ev
}

fn console(src: &str) -> Vec<String> {
    let ev = run(src);
    assert!(ev.errors().is_empty(), "errors: {:?}", ev.errors());
    ev.console().to_vec()
}

fn has_warning(ev: &Evaluator, needle: &str) -> bool {
    ev.warnings().iter().any(|w| w.contains(needle))
}

#[test]
fn literal_assigned_to_a_variable_is_callable() {
    assert_eq!(console("f = function(x) x * 2; echo(f(3));"), ["ECHO: 6"]);
}

#[test]
fn literal_called_directly() {
    assert_eq!(console("echo((function(x) x + 1)(2));"), ["ECHO: 3"]);
}

#[test]
fn closures_capture_the_defining_scope() {
    let src = "function adder(n) = function(x) x + n;\n\
               add2 = adder(2);\n\
               add5 = adder(5);\n\
               echo(add2(3), add5(3), adder(1)(1));";
    assert_eq!(console(src), ["ECHO: 5, 8, 2"]);
    // A let-bound capture outlives the let.
    assert_eq!(
        console("g = let(k = 10) function(x) x * k; echo(g(4));"),
        ["ECHO: 40"]
    );
}

#[test]
fn closures_see_later_bindings_of_their_scope() {
    // Upstream captures the context itself, not a snapshot of it.
    assert_eq!(
        console("f = function() later; later = 3; echo(f());"),
        ["ECHO: 3"]
    );
}

#[test]
fn recursive_literal_through_its_variable() {
    let src = "fact = function(n) n <= 1 ? 1 : n * fact(n - 1); echo(fact(5));";
    assert_eq!(console(src), ["ECHO: 120"]);
}

#[test]
fn functions_are_values_passed_and_returned() {
    let src = "function apply(g, v) = g(v);\n\
               function compose(f, g) = function(x) f(g(x));\n\
               sq = function(x) x * x;\n\
               inc = function(x) x + 1;\n\
               echo(apply(sq, 4), compose(sq, inc)(2), [for (h = [sq, inc]) h(3)]);";
    assert_eq!(console(src), ["ECHO: 16, 9, [9, 4]"]);
}

#[test]
fn is_function_only_for_function_values() {
    let ev = run("f = function(x) x; function g(x) = x;\n\
                  echo(is_function(f), is_function(5), is_function(g));");
    // A named definition is not a variable (upstream keeps functions and
    // variables in separate namespaces).
    assert_eq!(ev.console(), ["ECHO: true, false, false"]);
    assert!(has_warning(&ev, "Ignoring unknown variable 'g'"));
}

#[test]
fn printing_matches_upstream() {
    let src = "f = function(x) x * 2;\n\
               echo(f);\n\
               echo(str(f));\n\
               echo([f]);\n\
               echo(function(a, b = [1, 2]) a ? b : -a);\n\
               echo(function() let(c = 1) c + \"s\");";
    assert_eq!(
        console(src),
        [
            "ECHO: function(x) (x * 2)",
            "ECHO: \"function(x) (x * 2)\"",
            "ECHO: [function(x) (x * 2)]",
            "ECHO: function(a, b = [1, 2]) (a ? b : -a)",
            "ECHO: function() let(c = 1) (c + \"s\")",
        ]
    );
}

#[test]
fn equality_is_identity() {
    let src = "f = function(x) x; g = f;\n\
               echo(f == g, f != g, f == function(x) x, f == 1);";
    assert_eq!(console(src), ["ECHO: true, false, false, false"]);
}

#[test]
fn function_values_are_truthy_and_have_a_type_name() {
    let ev = run("f = function() 1; echo(f ? \"yes\" : \"no\"); x = f + 1;");
    assert_eq!(ev.console(), ["ECHO: \"yes\""]);
    assert!(has_warning(&ev, "undefined operation (function + number)"));
}

#[test]
fn a_definition_wins_over_a_variable_in_the_same_scope() {
    // ScopeContext::lookup_local_function checks the scope's definitions
    // before ContextFrame's function-valued variables.
    let src = "function f(x) = \"def\"; f = function(x) \"var\"; echo(f(1));";
    assert_eq!(console(src), ["ECHO: \"def\""]);
}

#[test]
fn an_inner_function_variable_shadows_an_outer_definition() {
    let src = "function f() = \"outer\";\n\
               echo(let(f = function() \"inner\") f());\n\
               echo(let(f = 5) f());";
    // A non-function variable does not shadow: the lookup moves on.
    assert_eq!(console(src), ["ECHO: \"inner\"", "ECHO: \"outer\""]);
}

#[test]
fn a_function_value_shadows_a_builtin() {
    assert_eq!(console("sin = function(x) 42; echo(sin(1));"), ["ECHO: 42"]);
}

#[test]
fn special_variable_functions_resolve_dynamically() {
    let src = "function call_it() = $op(2);\n\
               echo(let($op = function(x) x * 10) call_it());";
    assert_eq!(console(src), ["ECHO: 20"]);
}

#[test]
fn defaults_evaluate_in_the_defining_scope() {
    let src = "y = 10; f = function(x = y) x; echo(f(), f(1));";
    assert_eq!(console(src), ["ECHO: 10, 1"]);
}

#[test]
fn calling_a_non_function_warns() {
    let ev = run("x = 5; echo(x(1));");
    assert_eq!(ev.console(), ["ECHO: undef"]);
    assert!(has_warning(&ev, "Ignoring unknown function 'x'"));
    let ev = run("echo([1](0));");
    assert_eq!(ev.console(), ["ECHO: undef"]);
    assert!(has_warning(&ev, "Can't call function on vector"));
}

#[test]
fn tail_calls_through_function_values_run_in_constant_stack() {
    // Well past MAX_CALL_DEPTH: only tail-call elimination gets here.
    let src = "count = function(n, acc = 0) n == 0 ? acc : count(n - 1, acc + 1);\n\
               echo(count(100000));";
    assert_eq!(console(src), ["ECHO: 100000"]);
}

#[test]
fn deep_non_tail_recursion_is_an_error() {
    let ev = run("g = function(n) n == 0 ? 0 : 1 + g(n - 1); x = g(100000);");
    assert!(
        ev.errors().iter().any(|e| e
            .message
            .contains("recursion limit exceeded calling function 'g'")),
        "{:?}",
        ev.errors()
    );
}

#[test]
fn literal_in_a_module_body() {
    let src = "module m(k) { f = function(x) x * k; echo(f(2)); }\nm(3);\nm(4);";
    assert_eq!(console(src), ["ECHO: 6", "ECHO: 8"]);
}
