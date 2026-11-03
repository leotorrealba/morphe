//! Control flow (`if`, `for`, `intersection_for`) producing CSG
//! group nodes.
//!
//! Every rule here is transcribed from upstream openscad
//! src/core/control.cc (builtin_if/builtin_for/builtin_intersection_for)
//! and src/core/Expression.cc (LcFor::forEach / doForEach) (master, fetched
//! 2026-07-02):
//!
//! - `builtin_if`: the condition coerces via `toBool`; the taken branch's
//!   scope instantiates in a fresh scope over the calling context
//!   (`Children(scope, context).instantiate(lazyUnionNode(inst))`) and
//!   wraps in a group. A false condition without an else yields no node.
//! - `builtin_for`/`builtin_intersection_for` iterate via `doForEach`:
//!   ranges (with a 1,000,000-step guard), vectors (elements), strings
//!   (characters); undef iterates zero times; any other value iterates
//!   once, bound as-is. Multiple bindings recurse — cartesian order, later
//!   bindings see earlier loop variables. The node (group / intersection)
//!   is produced even when empty; zero bindings iterate zero times.

use morphe_eval::csg::CsgNode;
use morphe_eval::eval::{Evaluator, MAX_INST_DEPTH, MAX_MODULE_DEPTH};
use morphe_eval::value::Value;
use morphe_lang::ast::Modifiers;
use morphe_lang::parser::parse_program;

fn n(x: f64) -> Value {
    Value::Number(x)
}

/// Parse and evaluate a whole program, returning the root node and the
/// evaluator (for its warning/error/console sinks).
fn eval(src: &str) -> (CsgNode, Evaluator) {
    let mut evaluator = Evaluator::new();
    evaluator.set_source(src);
    let program = parse_program(src).expect("program should parse");
    let root = evaluator.eval_program(&program);
    (root, evaluator)
}

fn has_warning(evaluator: &Evaluator, needle: &str) -> bool {
    evaluator.warnings().iter().any(|w| w.contains(needle))
}

/// The children of a Group node.
fn group_children(node: &CsgNode) -> &[CsgNode] {
    match node {
        CsgNode::Group { children, .. } => children,
        other => panic!("expected a group node, got {other:?}"),
    }
}

/// The name of a Call node.
fn call_name(node: &CsgNode) -> &str {
    match node {
        CsgNode::Call { name, .. } => name,
        other => panic!("expected a call node, got {other:?}"),
    }
}

/// The first (evaluated) argument of a Call node.
fn first_arg(node: &CsgNode) -> &Value {
    match node {
        CsgNode::Call { args, .. } => &args[0].1,
        other => panic!("expected a call node, got {other:?}"),
    }
}

/// The first arguments of all Call children of a group-like node's child
/// list.
fn first_args(children: &[CsgNode]) -> Vec<Value> {
    children.iter().map(|c| first_arg(c).clone()).collect()
}

// --------------------------------------------------------------------- if

#[test]
fn if_true_instantiates_then_branch_as_group() {
    // builtin_if: a true condition instantiates inst->scope wrapped in
    // lazyUnionNode (a GroupNode without the lazy-union feature).
    let (root, evaluator) = eval("if (true) render(1);");
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    let branch = group_children(&children[0]);
    assert_eq!(branch.len(), 1);
    assert_eq!(call_name(&branch[0]), "render");
    assert!(evaluator.warnings().is_empty());
    assert!(evaluator.errors().is_empty());
}

#[test]
fn if_false_without_else_produces_no_node() {
    // builtin_if: "if" with failed condition and no "else" returns nullptr
    // — and nothing of the branch evaluates.
    let (root, evaluator) = eval("if (false) render(nope);");
    assert!(group_children(&root).is_empty());
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn if_false_with_else_instantiates_else_branch() {
    let (root, evaluator) = eval("if (false) render(1); else fill(2);");
    let branch = group_children(&group_children(&root)[0]);
    assert_eq!(branch.len(), 1);
    assert_eq!(call_name(&branch[0]), "fill");
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn if_condition_coerces_via_to_bool() {
    // arguments[0]->toBool(): non-zero numbers are truthy, 0 / "" / [] are
    // falsy.
    let (root, _) = eval("if (2) render(1);");
    assert_eq!(group_children(&root).len(), 1);
    let (root, _) = eval("if (0) render(1);");
    assert!(group_children(&root).is_empty());
    let (root, _) = eval("if (\"\") render(1); else fill(1);");
    let branch = group_children(&group_children(&root)[0]);
    assert_eq!(call_name(&branch[0]), "fill");
    let (root, _) = eval("if ([]) render(1);");
    assert!(group_children(&root).is_empty());
}

#[test]
fn if_untaken_branch_never_evaluates() {
    // Only the taken scope instantiates: no echo output, no unknown-module
    // warning from the dead branch.
    let (_, evaluator) = eval("if (false) echo(\"dead\"); else echo(\"live\");");
    assert_eq!(evaluator.console(), ["ECHO: \"live\""]);

    let (_, evaluator) = eval("if (true) render(1); else mystery(1);");
    assert!(!has_warning(&evaluator, "Ignoring unknown module"));
}

#[test]
fn if_branch_bindings_do_not_leak() {
    // The branch body evaluates in a fresh scope over the calling context
    // (Children::instantiate's ScopeContext), so its assignments vanish.
    let (root, evaluator) = eval("if (true) { a = 1; render(a); } fill(a);");
    let branch = group_children(&group_children(&root)[0]);
    assert_eq!(first_arg(&branch[0]), &n(1.0));
    assert!(has_warning(&evaluator, "Ignoring unknown variable 'a'"));
}

#[test]
fn if_with_empty_block_still_produces_a_group() {
    // builtin_if has no empty-children check (unlike echo/assert): the
    // taken branch's group is returned even with no child geometry.
    let (root, _) = eval("if (true) {}");
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    assert!(group_children(&children[0]).is_empty());
}

#[test]
fn else_if_chain_nests_groups() {
    // The else scope holds the nested if instantiation, so an else-if
    // chain produces one group per taken level.
    let (root, evaluator) = eval("if (false) render(1); else if (true) fill(2);");
    let outer = group_children(&group_children(&root)[0]);
    assert_eq!(outer.len(), 1);
    let inner = group_children(&outer[0]);
    assert_eq!(call_name(&inner[0]), "fill");
    assert!(evaluator.warnings().is_empty());
}

// -------------------------------------------------------------------- for

#[test]
fn for_over_range_instantiates_body_per_value() {
    let (root, evaluator) = eval("for (i = [0 : 2]) render(i);");
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    let body = group_children(&children[0]);
    assert_eq!(first_args(body), [n(0.0), n(1.0), n(2.0)]);
    assert!(evaluator.warnings().is_empty());
    assert!(evaluator.errors().is_empty());
}

#[test]
fn for_over_range_with_negative_step_descends() {
    let (root, evaluator) = eval("for (i = [3 : -1 : 1]) render(i);");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(first_args(body), [n(3.0), n(2.0), n(1.0)]);
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn for_zero_step_range_warns_too_many_elements_and_is_empty() {
    // RangeType::numValues returns UINT32_MAX for a zero step with
    // begin < end, so doForEach's `steps >= 1000000` guard fires: it warns
    // "Bad range parameter in for statement: too many elements" and
    // iterates zero times — the group node is still produced.
    let (root, evaluator) = eval("for (i = [0 : 0 : 1]) render(i);");
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    assert!(group_children(&children[0]).is_empty());
    assert!(has_warning(
        &evaluator,
        "Bad range parameter in for statement: too many elements"
    ));
}

#[test]
fn for_empty_range_produces_empty_group_without_warning() {
    // [2 : 1] has numValues 0 (begin > end with positive step): under the
    // step guard, zero iterations, no warning — but the group node exists.
    let (root, evaluator) = eval("for (i = [2 : 1]) render(i);");
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    assert!(group_children(&children[0]).is_empty());
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn for_over_vector_binds_each_element() {
    // doForEach VECTOR branch: elements bind as-is, heterogeneous values
    // included.
    let (root, evaluator) = eval("for (v = [5, \"a\", [1, 2]]) render(v);");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(
        first_args(body),
        [
            n(5.0),
            Value::String("a".into()),
            Value::from(vec![n(1.0), n(2.0)])
        ]
    );
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn for_over_string_iterates_characters() {
    // doForEach STRING branch: one iteration per (utf8) character, each
    // bound as a one-character string.
    let (root, _) = eval("for (c = \"ab\") render(c);");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(
        first_args(body),
        [Value::String("a".into()), Value::String("b".into())]
    );
}

#[test]
fn for_scalar_iterates_once_binding_the_value() {
    // doForEach's final branch: any value that is not a range, vector,
    // object, string, or undef iterates exactly once with the value bound.
    let (root, evaluator) = eval("for (i = 7) render(i);");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(first_args(body), [n(7.0)]);
    assert!(evaluator.warnings().is_empty());

    let (root, _) = eval("for (b = true) render(b);");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(first_args(body), [Value::Bool(true)]);
}

#[test]
fn for_undef_iterates_zero_times_silently() {
    // doForEach: `else if (type != UNDEFINED)` — undef falls through every
    // branch, iterating zero times with no warning.
    let (root, evaluator) = eval("for (i = undef) render(i);");
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    assert!(group_children(&children[0]).is_empty());
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn for_multiple_bindings_iterate_in_cartesian_order() {
    // doForEach recurses per binding: the first binding is the outer loop.
    let (root, evaluator) = eval("for (i = [0 : 1], j = [10, 20]) render(i + j);");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(first_args(body), [n(10.0), n(20.0), n(11.0), n(21.0)]);
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn for_later_bindings_see_earlier_loop_variables() {
    // Each binding's expression evaluates in the accumulated iteration
    // context (doForEach passes the inner context down), so j's range can
    // reference i.
    let (root, evaluator) = eval("for (i = [1 : 2], j = [0 : i]) render(i * 10 + j);");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(
        first_args(body),
        [n(10.0), n(11.0), n(20.0), n(21.0), n(22.0)]
    );
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn for_unnamed_binding_iterates_without_binding() {
    // parser.y feeds `for` the call-`arguments` grammar, so a positional
    // entry has no name; doForEach still iterates it (upstream binds the
    // empty name, which no lookup can reach).
    let (root, evaluator) = eval("for ([0, 1]) render(9);");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(first_args(body), [n(9.0), n(9.0)]);
    assert!(evaluator.errors().is_empty());
}

#[test]
fn nested_for_produces_nested_groups() {
    let (root, evaluator) = eval("for (i = [0 : 1]) for (j = [0 : 1]) render(i * 2 + j);");
    let outer = group_children(&group_children(&root)[0]);
    assert_eq!(outer.len(), 2);
    assert_eq!(first_args(group_children(&outer[0])), [n(0.0), n(1.0)]);
    assert_eq!(first_args(group_children(&outer[1])), [n(2.0), n(3.0)]);
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn for_loop_variable_does_not_leak() {
    let (_, evaluator) = eval("for (i = [0 : 1]) render(i); fill(i);");
    assert!(has_warning(&evaluator, "Ignoring unknown variable 'i'"));
}

#[test]
fn for_loop_variable_shadows_outer_binding() {
    let (root, evaluator) = eval("i = 99; for (i = [5 : 5]) render(i); fill(i);");
    let children = group_children(&root);
    let body = group_children(&children[0]);
    assert_eq!(first_args(body), [n(5.0)]);
    // After the loop the outer binding is untouched.
    assert_eq!(first_arg(&children[1]), &n(99.0));
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn for_body_assignment_shadows_loop_variable_in_body_scope() {
    // The body instantiates in its own ScopeContext over the iteration
    // context, so a body assignment shadows the loop variable there
    // without touching the binding frame.
    let (root, evaluator) = eval("for (i = [0 : 0]) { i = 5; render(i); }");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(first_args(body), [n(5.0)]);
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn for_body_scope_assignments_do_not_leak_across_iterations() {
    // Each iteration gets fresh frames: a body binding from iteration 0
    // must not be visible before its assignment in iteration 1 (references
    // before a name's first assignment fall through to outer scopes).
    let (root, evaluator) = eval("a = 100; for (i = [0 : 1]) { b = a + i; render(b); }");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(first_args(body), [n(100.0), n(101.0)]);
    assert!(evaluator.warnings().is_empty());
}

// --------------------------------------------------------- intersection_for

#[test]
fn intersection_for_produces_intersection_node() {
    // builtin_intersection_for: the same doForEach iteration, but the
    // produced node is an AbstractIntersectionNode.
    let (root, evaluator) = eval("intersection_for (i = [0 : 2]) render(i);");
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    let CsgNode::Intersection { children: body, .. } = &children[0] else {
        panic!("expected an intersection node, got {:?}", children[0]);
    };
    assert_eq!(first_args(body), [n(0.0), n(1.0), n(2.0)]);
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn intersection_for_with_empty_iteration_is_still_an_intersection_node() {
    let (root, _) = eval("intersection_for (i = [2 : 1]) render(i);");
    let CsgNode::Intersection { children, .. } = &group_children(&root)[0] else {
        panic!("expected an intersection node");
    };
    assert!(children.is_empty());
}

// -------------------------------------------------------------- modifiers

#[test]
fn modifiers_are_carried_onto_control_flow_nodes() {
    let (root, _) = eval(
        "#if (true) render(1); \
         !for (i = [0 : 0]) render(i); \
         %intersection_for (i = [0 : 0]) render(i);",
    );
    let children = group_children(&root);
    let CsgNode::Group { modifiers, .. } = &children[0] else {
        panic!("expected a group node");
    };
    assert_eq!(
        *modifiers,
        Modifiers {
            debug: true,
            ..Modifiers::default()
        }
    );
    let CsgNode::Group { modifiers, .. } = &children[1] else {
        panic!("expected a group node");
    };
    assert!(modifiers.root);
    let CsgNode::Intersection { modifiers, .. } = &children[2] else {
        panic!("expected an intersection node");
    };
    assert!(modifiers.background);
}

#[test]
fn disabled_control_flow_never_evaluates() {
    // parser.y deletes `*`-disabled instantiations at parse time, so no
    // iteration, no body side effects, and no node.
    let (root, evaluator) = eval("*for (i = [0 : 2]) echo(i); *if (true) echo(\"x\");");
    assert!(group_children(&root).is_empty());
    assert!(evaluator.console().is_empty());
    assert!(evaluator.warnings().is_empty());
}

// ---------------------------------------------------------- special vars

#[test]
fn dollar_variables_propagate_dynamically_into_loop_bodies() {
    // The loop's binding and body frames chain dynamically to the caller,
    // so a $-binding in the for body is visible to a user module called
    // from it (upstream special-variable lookup walks the live frame
    // stack).
    let (root, evaluator) =
        eval("module probe() render($fn); for (i = [0 : 0]) { $fn = 32; probe(); }");
    let body = group_children(&group_children(&root)[0]);
    let probe = group_children(&body[0]);
    assert_eq!(first_args(probe), [n(32.0)]);
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn dollar_loop_variable_binds_dynamically() {
    // A $-named loop variable binds in the iteration frame's config map,
    // so it flows into calls made from the body.
    let (root, evaluator) = eval("module probe() render($fn); for ($fn = [8, 16]) probe();");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(body.len(), 2);
    assert_eq!(first_args(group_children(&body[0])), [n(8.0)]);
    assert_eq!(first_args(group_children(&body[1])), [n(16.0)]);
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn dollar_variables_propagate_into_if_branches() {
    let (root, evaluator) = eval("module probe() render($fn); if (true) { $fn = 24; probe(); }");
    let branch = group_children(&group_children(&root)[0]);
    let probe = group_children(&branch[0]);
    assert_eq!(first_args(probe), [n(24.0)]);
    assert!(evaluator.warnings().is_empty());
}

// ------------------------------------------------------------ depth guard

#[test]
fn module_recursion_through_control_flow_trips_module_depth_guard() {
    // Control flow adds frames but no call depth: a module recursing
    // through if/for still trips MAX_MODULE_DEPTH (upstream StackCheck
    // covers all of these uniformly). The big dedicated stack keeps the
    // native cost of the nested evaluator frames irrelevant to the
    // harness thread size — the test must observe the guard, never a real
    // overflow.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let src = "module r() { if (true) for (i = [0 : 0]) r(); } r();";
            let (_, evaluator) = eval(src);
            assert_eq!(evaluator.errors().len(), 1);
            assert!(evaluator.errors()[0]
                .message
                .contains("recursion limit exceeded calling module 'r'"));
            assert_eq!(MAX_MODULE_DEPTH, 512);
        })
        .expect("spawn")
        .join()
        .expect("join");
}

#[test]
fn deep_module_recursion_survives_a_small_caller_stack() {
    // The desktop app runs eval on 2 MiB `spawn_blocking` threads: a
    // 510-deep legal module recursion used to overflow that stack (process
    // abort). eval_program now runs on its own large-stack thread, so the
    // caller's 2 MiB is irrelevant — both the legal-depth program and the
    // guard-tripping one must finish cleanly.
    let (deep_ok, guard_msg) = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let src = "module m(n) if (n > 0) translate([1,0,0]) m(n-1); m(510);";
            let (_, evaluator) = eval(src);
            let deep_ok = evaluator.errors().is_empty();
            let src = "module r() { if (true) for (i = [0 : 0]) r(); } r();";
            let (_, evaluator) = eval(src);
            let guard_msg = evaluator.errors().first().map(|e| e.message.clone());
            (deep_ok, guard_msg)
        })
        .expect("spawn")
        .join()
        .expect("eval must not overflow a 2 MiB caller stack");
    assert!(
        deep_ok,
        "m(510) is within MAX_MODULE_DEPTH and must evaluate"
    );
    assert!(guard_msg
        .expect("runaway recursion must error")
        .contains("recursion limit exceeded calling module 'r'"));
}

#[test]
fn statement_nesting_too_deep_errors_with_span() {
    // A dedicated big stack makes the native cost of MAX_INST_DEPTH
    // evaluator frames irrelevant to the harness thread size: the test
    // must observe the guard, never a real overflow. 32MB (was 16) — the
    // import match arm grew eval_program's DEBUG-codegen frame past 16MB's
    // margin at MAX_INST_DEPTH=2048 (measured: CsgNode itself stayed 152
    // bytes; the budget follows the frame, not the enum).
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(statement_nesting_too_deep_body)
        .expect("spawn")
        .join()
        .expect("join");
}

fn statement_nesting_too_deep_body() {
    use morphe_lang::ast::{Expr, ExprKind, Stmt, StmtKind};
    use morphe_lang::lexer::Span;

    // `if (true) if (true) ... render();` nested past MAX_INST_DEPTH, built
    // programmatically — if/for bodies recurse natively through the
    // evaluator without consuming call depth, so only the statement
    // nesting guard can stop this.
    let span = Span { start: 0, end: 1 };
    let mut stmt = Stmt {
        kind: StmtKind::ModuleCall {
            name: "render".to_string(),
            args: Vec::new(),
            modifiers: Modifiers::default(),
            children: Vec::new(),
        },
        span,
    };
    for _ in 0..MAX_INST_DEPTH + 100 {
        stmt = Stmt {
            kind: StmtKind::If {
                cond: Expr {
                    kind: ExprKind::Bool(true),
                    span,
                },
                then_branch: Box::new(stmt),
                else_branch: None,
                modifiers: Modifiers::default(),
            },
            span,
        };
    }

    let mut evaluator = Evaluator::new();
    evaluator.eval_program(std::slice::from_ref(&stmt));
    assert_eq!(evaluator.errors().len(), 1);
    let error = &evaluator.errors()[0];
    assert!(
        error.message.contains("statement nesting too deep"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.span, span);

    // The depth counter unwinds: the evaluator stays usable.
    let program = parse_program("if (true) render(1);").expect("parse");
    evaluator.eval_program(&program);
    assert_eq!(evaluator.errors().len(), 1);
}

// ------------------------------------------------------------- $children

#[test]
fn control_flow_counts_toward_dollar_children() {
    // for/if are module instantiations upstream, so each counts once
    // toward $children regardless of iteration count.
    let (_, evaluator) = eval(
        "module m() echo($children); \
         m() { for (i = [0 : 9]) render(i); if (false) fill(1); }",
    );
    assert_eq!(evaluator.console(), ["ECHO: 2"]);
}
