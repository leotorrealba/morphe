//! Vectors and strings are reference-counted and shared, never deep-copied
//! on lookup, binding or indexing — the analogue of upstream openscad's
//! `VectorType` / `str_utf8_wrapper`, which wrap a `shared_ptr`
//! (`src/core/Value.h`). Without sharing, `v[i]` copies `v` on every lookup
//! (quadratic loops) and `let(a = d(n-1)) [a, a]` doubles memory per level.

use std::sync::Arc;
use std::time::{Duration, Instant};

use morphe_eval::eval::Evaluator;
use morphe_eval::value::Value;
use morphe_lang::parser::{parse_expr, parse_program};

fn eval_program(src: &str) -> Evaluator {
    let mut evaluator = Evaluator::new();
    let program = parse_program(src).expect("program should parse");
    evaluator.eval_program(&program);
    evaluator
}

fn eval_expr(src: &str) -> Value {
    let mut evaluator = Evaluator::new();
    let expr = parse_expr(src).expect("expression should parse");
    evaluator.eval_expr(&expr)
}

fn as_vector(value: &Value) -> &Arc<Vec<Value>> {
    match value {
        Value::Vector(items) => items,
        other => panic!("expected a vector, got {other:?}"),
    }
}

#[test]
fn variable_lookup_shares_the_vector() {
    let mut evaluator = eval_program("v = [1, 2, 3];");
    let a = evaluator.lookup("v");
    let b = evaluator.lookup("v");
    assert!(Arc::ptr_eq(as_vector(&a), as_vector(&b)));
}

#[test]
fn variable_lookup_shares_the_string() {
    let mut evaluator = eval_program("s = \"hello\";");
    let (Value::String(a), Value::String(b)) = (&evaluator.lookup("s"), &evaluator.lookup("s"))
    else {
        panic!("expected strings");
    };
    assert!(Arc::ptr_eq(a, b));
}

#[test]
fn let_binding_reused_twice_is_stored_once() {
    let value = eval_expr("let(a = [1, 2, 3]) [a, a]");
    let outer = as_vector(&value);
    assert_eq!(outer.len(), 2);
    assert!(Arc::ptr_eq(as_vector(&outer[0]), as_vector(&outer[1])));
}

#[test]
fn indexing_a_nested_vector_shares_the_element() {
    let mut evaluator = eval_program("m = [[1, 2], [3, 4]]; r = m[1];");
    let m = evaluator.lookup("m");
    let r = evaluator.lookup("r");
    assert!(Arc::ptr_eq(as_vector(&as_vector(&m)[1]), as_vector(&r)));
}

#[test]
fn doubling_recursion_is_linear_in_depth() {
    // Upstream is linear here thanks to shared_ptr; a deep-copying value
    // model needs 2^22 leaves (gigabytes).
    let mut evaluator =
        eval_program("function d(n) = n == 0 ? [0] : let(a = d(n - 1)) [a, a]; x = d(22);");
    assert!(evaluator.errors().is_empty(), "{:?}", evaluator.errors());
    let mut value = evaluator.lookup("x");
    for _ in 0..22 {
        let next = {
            let items = as_vector(&value);
            assert_eq!(items.len(), 2);
            assert!(Arc::ptr_eq(as_vector(&items[0]), as_vector(&items[1])));
            items[0].clone()
        };
        value = next;
    }
    assert_eq!(value, Value::from(vec![Value::Number(0.0)]));
}

/// Wall-clock guard for the 40k-element indexing loop that used to be
/// quadratic (~16 s in release). It now takes milliseconds even in a debug
/// build; the 5 s bound is generous so a loaded CI machine cannot flake,
/// while the deterministic `Arc::ptr_eq` tests above pin the mechanism.
#[test]
fn indexing_loop_over_large_vector_is_fast() {
    let start = Instant::now();
    let evaluator = eval_program(
        "v = [for (i = [0:39999]) i]; s = [for (i = [0:39999]) v[i]]; echo(len(s), s[39999]);",
    );
    let elapsed = start.elapsed();
    assert_eq!(evaluator.console(), ["ECHO: 40000, 39999"]);
    assert!(
        elapsed < Duration::from_secs(5),
        "indexing loop took {elapsed:?}"
    );
}
