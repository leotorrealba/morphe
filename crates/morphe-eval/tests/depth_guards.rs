//! The depth guards ([`MAX_CALL_DEPTH`], [`MAX_MODULE_DEPTH`],
//! [`MAX_EXPR_DEPTH`], [`MAX_INST_DEPTH`]) are sized so that the deepest
//! legal recursion — every guard tripping at once — needs well under the
//! pipeline thread's stack. These programs drive the guards to their limits
//! on a thread with only HALF of [`PIPELINE_STACK_BYTES`]: if a guard is
//! raised (or a frame grows) past the 2x safety margin, the process aborts
//! with a stack overflow here, in a debug build, instead of in the field.

use morphe_eval::eval::{Evaluator, MAX_CALL_DEPTH, MAX_EXPR_DEPTH, MAX_MODULE_DEPTH};
use morphe_eval::stack::{with_stack_size, PIPELINE_STACK_BYTES};
use morphe_lang::parser::parse_program;

/// Evaluate `src` with half the pipeline stack, returning the error messages.
fn errors_on_half_stack(src: &str) -> Vec<String> {
    let program = parse_program(src).expect("program should parse");
    let mut evaluator = Evaluator::new();
    with_stack_size(PIPELINE_STACK_BYTES / 2, || {
        evaluator.eval_program(&program);
    });
    evaluator
        .errors()
        .iter()
        .map(|e| e.message.clone())
        .collect()
}

/// Module recursion close to [`MAX_MODULE_DEPTH`] (three statement levels
/// per call), with `leaf` evaluated at the bottom.
fn under_module_recursion(functions: &str, leaf: &str) -> String {
    let depth = MAX_MODULE_DEPTH - 2;
    format!(
        "{functions}\n\
         module m(n) for (i = [0]) if (n > 0) m(n - 1); else echo({leaf});\n\
         m({depth});"
    )
}

#[test]
fn guard_limits_are_the_documented_ones() {
    assert_eq!(MAX_CALL_DEPTH, 8_000);
    assert_eq!(MAX_EXPR_DEPTH, 20_000);
    assert_eq!(MAX_MODULE_DEPTH, 512);
}

#[test]
fn guards_trip_before_half_the_pipeline_stack_call_heavy() {
    // One expression level per call (the argument of a tail call), so the
    // call cap trips with the most call frames on the stack.
    let src = under_module_recursion(
        "function h(x) = x; function g(n) = n == 0 ? 0 : h(g(n - 1));",
        "g(1000000)",
    );
    let errors = errors_on_half_stack(&src);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("recursion limit exceeded calling function 'g'")),
        "{errors:?}"
    );
}

#[test]
fn guards_trip_before_half_the_pipeline_stack_mixed() {
    // Three expression levels per call (two of them builtin calls): the call
    // and expression caps trip together.
    let src = under_module_recursion(
        "function g(n) = n == 0 ? 0 : abs(abs(g(n - 1)));",
        "g(1000000)",
    );
    let errors = errors_on_half_stack(&src);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("expression nesting too deep")),
        "{errors:?}"
    );
}

#[test]
fn guards_trip_before_half_the_pipeline_stack_expression_heavy() {
    // Builtin calls are the costliest expression levels without a user
    // call: the expression cap trips with the most of them on the stack.
    let src = under_module_recursion(
        "function g(n) = n == 0 ? 0 : abs(abs(abs(abs(abs(abs(abs(abs(g(n - 1)))))))));",
        "g(1000000)",
    );
    let errors = errors_on_half_stack(&src);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("expression nesting too deep")),
        "{errors:?}"
    );
}

#[test]
fn runaway_module_recursion_trips_before_half_the_pipeline_stack() {
    let errors =
        errors_on_half_stack("module m(n) if (n > 0) translate([1, 0, 0]) m(n - 1); m(1000000);");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("recursion limit exceeded calling module 'm'")),
        "{errors:?}"
    );
}
