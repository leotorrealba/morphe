//! `echo(...)` / `assert(...)` as expressions, and expression precedence.
//!
//! Upstream: `src/core/Expression.cc` (`Echo::evaluate`/`evaluateStep`,
//! `Assert::evaluate`/`evaluateStep`, `simplify_function_body`) and the
//! expression grammar of `src/core/parser.y` (`expr`, `logic_or`,
//! `logic_and`, `equality`, `comparison`, `addition`, `multiplication`,
//! `unary`, `exponent`).

use morphe_eval::eval::Evaluator;
use morphe_lang::parser::parse_program;

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

// ------------------------------------------------------------------- echo

#[test]
fn echo_expression_prints_then_yields_its_body() {
    assert_eq!(
        console("x = echo(\"hi\") 3; echo(x);"),
        ["ECHO: \"hi\"", "ECHO: 3"]
    );
}

#[test]
fn echo_expression_without_body_is_undef() {
    assert_eq!(
        console("y = echo(\"only\", n = 2); echo(y);"),
        ["ECHO: \"only\", n = 2", "ECHO: undef"]
    );
}

#[test]
fn echo_inside_a_function_runs_per_call() {
    let src = "function f(x) = echo(str(\"f \", x)) x * 2;\necho(f(2) + f(3));";
    assert_eq!(console(src), ["ECHO: \"f 2\"", "ECHO: \"f 3\"", "ECHO: 10"]);
}

// ----------------------------------------------------------------- assert

#[test]
fn passing_assert_expression_yields_its_body() {
    assert_eq!(
        console("v = assert(true, \"never\") 5; echo(v, assert(1));"),
        ["ECHO: 5, undef"]
    );
}

#[test]
fn failing_assert_expression_is_an_error_and_skips_the_body() {
    let ev = run("v = assert(1 > 2, \"too small\") echo(\"body\") 5; echo(v);");
    assert_eq!(ev.errors().len(), 1);
    assert_eq!(
        ev.errors()[0].message,
        "Assertion '1 > 2' failed: \"too small\""
    );
    assert_eq!(ev.console(), ["ECHO: undef"]);
}

#[test]
fn let_echo_and_assert_chain() {
    let src = "z = let(a = 2) echo(a = a) assert(a > 1) a * 10; echo(z);";
    assert_eq!(console(src), ["ECHO: a = 2", "ECHO: 20"]);
}

#[test]
fn echo_and_assert_keep_the_tail_position() {
    // Well past MAX_CALL_DEPTH: the call after `assert(...)` must still be
    // eliminated as a tail call (upstream simplify_function_body steps
    // through Assert/Echo).
    let src = "function count(n, acc = 0) = n == 0 ? acc : assert(n > 0) count(n - 1, acc + 1);\n\
               echo(count(50000));";
    assert_eq!(console(src), ["ECHO: 50000"]);
    let src = "function f(n) = n == 0 ? echo(\"done\") 0 : f(n - 1);\necho(f(20000));";
    assert_eq!(console(src), ["ECHO: \"done\"", "ECHO: 0"]);
}

#[test]
fn echo_and_assert_statements_still_work() {
    let ev = run("echo(\"stmt\"); assert(true); assert(false, \"boom\");");
    assert_eq!(ev.console(), ["ECHO: \"stmt\""]);
    assert_eq!(ev.errors().len(), 1);
    assert!(ev.errors()[0].message.contains("boom"));
}

// ------------------------------------------------------------- precedence

#[test]
fn relational_binds_tighter_than_equality() {
    // parser.y `equality: equality EQ comparison`.
    assert_eq!(
        console("echo(1 < 2 == true, 2 > 1 != false, 1 + 2 == 3);"),
        ["ECHO: true, true, true"]
    );
}

#[test]
fn unary_binds_tighter_than_multiplication() {
    // parser.y `multiplication: multiplication '*' unary`: `!0 * 5` is
    // `(!0) * 5` = `true * 5`, an undefined operation.
    let ev = run("echo(!0 * 5, !(0 * 5));");
    assert_eq!(ev.console(), ["ECHO: undef, true"]);
    assert!(has_warning(&ev, "undefined operation (bool * number)"));
}

#[test]
fn power_binds_tighter_than_unary_minus() {
    // parser.y `unary: '-' unary | exponent` and `exponent: call '^' unary`.
    assert_eq!(
        console("echo(-2 ^ 2, 2 ^ -1, 2 ^ 3 ^ 2, -2 ^ -2);"),
        ["ECHO: -4, 0.5, 512, -0.25"]
    );
}

#[test]
fn chained_comparisons_are_left_associative() {
    let ev = run("echo(3 > 2 > 1);");
    assert_eq!(ev.console(), ["ECHO: undef"]);
    assert!(has_warning(&ev, "undefined operation (bool > number)"));
}
