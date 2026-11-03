//! User-defined module instantiation with children() support.
//!
//! Every rule here is transcribed from upstream openscad
//! src/core/UserModule.cc, Children.cc, control.cc (builtin
//! children/echo/assert), ScopeContext.cc (UserModuleContext), LocalScope.cc,
//! ModuleInstantiation.cc, Expression.cc (Assert::performAssert) and parser.y
//! (master, fetched 2026-07-02) — except the 512-deep module cap (upstream
//! guards module recursion with the same StackCheck stack probe it uses for
//! functions) and assert's failure scope (upstream throws
//! AssertionFailedException, aborting the whole evaluation; here the error is
//! recorded and only the assert's subtree is dropped).

use morphe_eval::csg::{CsgNode, Primitive};
use morphe_eval::eval::{Evaluator, MAX_MODULE_DEPTH};
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

// ------------------------------------------------------------ eval_program

#[test]
fn eval_program_returns_root_group() {
    let (root, evaluator) = eval("render(1); fill(2);");
    let children = group_children(&root);
    assert_eq!(children.len(), 2);
    assert_eq!(call_name(&children[0]), "render");
    assert_eq!(call_name(&children[1]), "fill");
    assert!(evaluator.warnings().is_empty());
    assert!(evaluator.errors().is_empty());
}

#[test]
fn builtin_call_records_evaluated_args_and_span() {
    // Builtin modules without a typed mapping (here `render`) stay
    // unresolved Call nodes; arguments evaluate eagerly in the calling
    // context (upstream Arguments.cc).
    let src = "x = 2; render(h = x + 1, 5);";
    let (root, _) = eval(src);
    let CsgNode::Call {
        name, args, span, ..
    } = &group_children(&root)[0]
    else {
        panic!("expected a call node");
    };
    assert_eq!(name, "render");
    assert_eq!(args, &vec![(Some("h".to_string()), n(3.0)), (None, n(5.0))]);
    assert!(&src[span.start..span.end].starts_with("render"));
}

#[test]
fn builtin_call_children_evaluate_into_the_node() {
    let (root, _) = eval("render([1, 0, 0]) render(1);");
    let CsgNode::Call { name, children, .. } = &group_children(&root)[0] else {
        panic!("expected a call node");
    };
    assert_eq!(name, "render");
    assert_eq!(children.len(), 1);
    assert_eq!(call_name(&children[0]), "render");
}

#[test]
fn unknown_module_warns_and_produces_no_node() {
    // Upstream Context::lookup_module: "Ignoring unknown module '%1$s'",
    // then ModuleInstantiation::evaluate returns nullptr.
    let (root, evaluator) = eval("mystery(1);");
    assert!(group_children(&root).is_empty());
    assert!(has_warning(&evaluator, "Ignoring unknown module 'mystery'"));
}

#[test]
fn top_level_block_is_scope_transparent() {
    // parser.y `statement: '{' inner_input '}'` adds statements (and
    // assignments) to the enclosing scope: no extra group node, and the
    // assignment binds in the file scope.
    let (root, evaluator) = eval("{ a = 1; render(a); } fill(a);");
    let children = group_children(&root);
    assert_eq!(children.len(), 2);
    assert_eq!(call_name(&children[0]), "render");
    assert_eq!(call_name(&children[1]), "fill");
    assert!(evaluator.warnings().is_empty());
}

// ------------------------------------------------------ user-defined modules

#[test]
fn simple_module_produces_group_of_its_body_calls() {
    // UserModule::instantiate wraps the body's nodes in a GroupNode.
    let (root, evaluator) = eval("module m() { render(1); fill(2); } m();");
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    let body = group_children(&children[0]);
    assert_eq!(body.len(), 2);
    assert_eq!(call_name(&body[0]), "render");
    assert_eq!(call_name(&body[1]), "fill");
    assert!(evaluator.warnings().is_empty());
    assert!(evaluator.errors().is_empty());
}

#[test]
fn single_statement_module_body() {
    let (root, _) = eval("module m() render(1); m();");
    let body = group_children(&group_children(&root)[0]);
    assert_eq!(body.len(), 1);
    assert_eq!(call_name(&body[0]), "render");
}

#[test]
fn module_parameters_bind() {
    let (root, _) = eval("module m(size, h = 2) render([size, h, 1]); m(5);");
    let body = group_children(&group_children(&root)[0]);
    let CsgNode::Call { args, .. } = &body[0] else {
        panic!("expected a call node");
    };
    assert_eq!(args[0].1, Value::from(vec![n(5.0), n(2.0), n(1.0)]));
}

#[test]
fn module_is_visible_before_its_definition() {
    // Definitions live in LocalScope maps looked up lazily, so a call
    // textually before the definition resolves.
    let (root, evaluator) = eval("m(); module m() render(1);");
    assert_eq!(group_children(&root).len(), 1);
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn module_body_sees_definition_scope() {
    // The module context's lexical parent is the defining context; scope
    // assignments bind before any instantiation runs (ScopeContext::init),
    // so even a textually later assignment is visible.
    let (root, evaluator) = eval("module m() render(x); m(); x = 7;");
    let body = group_children(&group_children(&root)[0]);
    let CsgNode::Call { args, .. } = &body[0] else {
        panic!("expected a call node");
    };
    assert_eq!(args[0].1, n(7.0));
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn module_body_assignments_do_not_leak() {
    let (_, evaluator) = eval("module m() { v = 9; render(v); } m(); fill(v);");
    assert!(has_warning(&evaluator, "Ignoring unknown variable 'v'"));
}

#[test]
fn module_body_literal_assignment_over_parameter_warns() {
    // ScopeContext::init: "Parameter %1$s is overwritten with a literal"
    // when a literal body assignment overwrites an existing local (the
    // parameter shares the module context frame with the body).
    let (root, evaluator) = eval("module m(p) { p = 3; render(p); } m(1);");
    let body = group_children(&group_children(&root)[0]);
    let CsgNode::Call { args, .. } = &body[0] else {
        panic!("expected a call node");
    };
    assert_eq!(args[0].1, n(3.0));
    assert!(has_warning(
        &evaluator,
        "Parameter 'p' is overwritten with a literal"
    ));
}

#[test]
fn nested_module_definitions_are_lexically_scoped() {
    // Context::lookup_module walks the context chain, so an inner module is
    // visible inside its defining scope but not outside it.
    let (root, evaluator) =
        eval("module outer() { module inner() fill(1); inner(); render(2); } outer();");
    let outer = group_children(&group_children(&root)[0]);
    assert_eq!(outer.len(), 2);
    let inner = group_children(&outer[0]);
    assert_eq!(call_name(&inner[0]), "fill");
    assert_eq!(call_name(&outer[1]), "render");
    assert!(evaluator.warnings().is_empty());

    let (root, evaluator) = eval("module outer() { module inner() fill(1); } outer(); inner();");
    assert_eq!(group_children(&root).len(), 1);
    assert!(has_warning(&evaluator, "Ignoring unknown module 'inner'"));
}

#[test]
fn module_body_defines_and_calls_local_function() {
    // Body definitions register in the module's own frame (LocalScope
    // maps looked up along the lexical chain) and are dropped when the
    // instantiation ends, so a second instantiation re-registers cleanly.
    let (root, evaluator) = eval("module m() { function h(x) = x + 1; render(h(1)); } m(); m();");
    let children = group_children(&root);
    assert_eq!(children.len(), 2);
    for group in children {
        let body = group_children(group);
        let CsgNode::Call { args, .. } = &body[0] else {
            panic!("expected a call node");
        };
        assert_eq!(args[0].1, n(2.0));
    }
    assert!(evaluator.warnings().is_empty());
    assert!(evaluator.errors().is_empty());
}

#[test]
fn module_arguments_evaluate_in_caller_scope() {
    let (root, _) = eval("module m(a) render(a); module wrap() { q = 4; m(q); } wrap();");
    let wrap = group_children(&group_children(&root)[0]);
    let m_body = group_children(&wrap[0]);
    let CsgNode::Call { args, .. } = &m_body[0] else {
        panic!("expected a call node");
    };
    assert_eq!(args[0].1, n(4.0));
}

#[test]
fn module_recursion_cap_errors_with_span() {
    // A dedicated big stack keeps the native cost of MAX_MODULE_DEPTH nested
    // evaluator frames irrelevant to the harness thread size (same pattern
    // as the expression-depth guard test): the test must observe the depth
    // guard, never a real overflow.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(module_recursion_cap_body)
        .expect("spawn")
        .join()
        .expect("join");
}

fn module_recursion_cap_body() {
    // Upstream guards module recursion with StackCheck and throws
    // "Recursion detected calling module '%1$s'"; morphe counts module
    // instantiations against MAX_MODULE_DEPTH = 512 instead.
    assert_eq!(MAX_MODULE_DEPTH, 512);
    let src = "module r() { r(); } r();";
    let (root, evaluator) = eval(src);
    assert_eq!(evaluator.errors().len(), 1);
    let error = &evaluator.errors()[0];
    assert!(
        error
            .message
            .contains("recursion limit exceeded calling module 'r'"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(&src[error.span.start..error.span.end], "r();");
    // The tree still unwinds into nested groups instead of crashing.
    assert_eq!(group_children(&root).len(), 1);
}

// ------------------------------------------------------------- children()

#[test]
fn children_all_instantiates_every_child() {
    let (root, evaluator) = eval("module wrap() children(); wrap() { render(1); fill(2); }");
    let wrap = group_children(&group_children(&root)[0]);
    // children() produces one group holding all instantiated children.
    let picked = group_children(&wrap[0]);
    assert_eq!(picked.len(), 2);
    assert_eq!(call_name(&picked[0]), "render");
    assert_eq!(call_name(&picked[1]), "fill");
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn children_index_selects_one_child() {
    let (root, _) = eval("module pick() children(1); pick() { render(1); fill(2); }");
    let pick = group_children(&group_children(&root)[0]);
    let picked = group_children(&pick[0]);
    assert_eq!(picked.len(), 1);
    assert_eq!(call_name(&picked[0]), "fill");
}

#[test]
fn children_vector_selects_in_order_with_duplicates() {
    // LocalScope::instantiateModules(indices) instantiates in index order,
    // duplicates included.
    let (root, _) = eval("module pick() children([1, 0, 0]); pick() { render(1); fill(2); }");
    let picked = group_children(&group_children(&group_children(&root)[0])[0]);
    assert_eq!(picked.len(), 3);
    assert_eq!(call_name(&picked[0]), "fill");
    assert_eq!(call_name(&picked[1]), "render");
    assert_eq!(call_name(&picked[2]), "render");
}

#[test]
fn children_range_selects_a_slice() {
    let (root, _) =
        eval("module pick() children([0 : 1]); pick() { render(1); fill(2); hull(3); }");
    let picked = group_children(&group_children(&group_children(&root)[0])[0]);
    assert_eq!(picked.len(), 2);
    assert_eq!(call_name(&picked[0]), "render");
    assert_eq!(call_name(&picked[1]), "fill");
}

#[test]
fn children_out_of_range_index_warns_and_yields_nothing() {
    // control.cc validChildIndex: "Children index (%1$d) out of bounds
    // (%2$d children)" and no node.
    let (root, evaluator) = eval("module pick() children(2); pick() { render(1); fill(2); }");
    assert!(group_children(&group_children(&root)[0]).is_empty());
    assert!(has_warning(
        &evaluator,
        "Children index (2) out of bounds (2 children)"
    ));

    let (_, evaluator) = eval("module pick() children(-1); pick() render(1);");
    assert!(has_warning(
        &evaluator,
        "Children index (-1) out of bounds (1 children)"
    ));
}

#[test]
fn children_vector_skips_invalid_entries_with_warnings() {
    // Out-of-range and non-number vector entries each warn and are skipped;
    // the remaining indices still instantiate.
    let (root, evaluator) =
        eval("module pick() children([0, 5, \"x\"]); pick() { render(1); fill(2); }");
    let picked = group_children(&group_children(&group_children(&root)[0])[0]);
    assert_eq!(picked.len(), 1);
    assert_eq!(call_name(&picked[0]), "render");
    assert!(has_warning(
        &evaluator,
        "Children index (5) out of bounds (2 children)"
    ));
    // Vector entries print via toString (unquoted), with a trailing period.
    assert!(has_warning(
        &evaluator,
        "Bad parameter type (x) for children, only accept: empty, number, vector, range."
    ));
}

#[test]
fn children_bad_parameter_type_warns_and_yields_nothing() {
    // The top-level bad-type branch prints via toEchoString (strings
    // quoted) and without a trailing period.
    let (root, evaluator) = eval("module pick() children(\"a\"); pick() render(1);");
    assert!(group_children(&group_children(&root)[0]).is_empty());
    assert!(has_warning(
        &evaluator,
        "Bad parameter type (\"a\") for children, only accept: empty, number, vector, range"
    ));
}

#[test]
fn children_outside_a_module_is_silently_ignored() {
    // control.cc builtin_children: a null user_module_children() returns
    // nullptr with no message.
    let (root, evaluator) = eval("children();");
    assert!(group_children(&root).is_empty());
    assert!(evaluator.warnings().is_empty());
    assert!(evaluator.errors().is_empty());
}

#[test]
fn children_does_not_support_child_modules() {
    // BuiltinModule::noChildren: "module children() does not support child
    // modules".
    let (_, evaluator) = eval("module m() children() render(1); m() fill(2);");
    assert!(has_warning(
        &evaluator,
        "module children() does not support child modules"
    ));
}

#[test]
fn children_with_only_assignments_does_not_warn() {
    // control.cc builtin_children calls BuiltinModule::noChildren
    // (module.cc), which warns iff inst->scope->hasChildren() —
    // LocalScope.h: !(moduleInstantiations.empty()) — so a child scope
    // holding only assignments (or nothing) never warns; one holding a
    // module instantiation does (see
    // children_does_not_support_child_modules).
    let (_, evaluator) = eval("module m() children() { a = 1; } m() render(1);");
    assert!(!has_warning(&evaluator, "does not support child modules"));

    let (_, evaluator) = eval("module m() children() { } m() render(1);");
    assert!(!has_warning(&evaluator, "does not support child modules"));

    // Control: an instantiation among assignments still warns.
    let (_, evaluator) = eval("module m() children() { a = 1; render(a); } m() fill(1);");
    assert!(has_warning(&evaluator, "does not support child modules"));
}

#[test]
fn children_undef_index_is_a_bad_parameter_type() {
    // Master control.cc keys "no index => all children" off
    // parameters.contains("index"); an explicit undef argument DOES bind
    // (Parameters::contains -> ContextFrame::lookup_local_variable finds
    // the undef entry), so children(undef) falls to the bad-parameter
    // branch, not the instantiate-all path.
    let (root, evaluator) = eval("module pick() children(undef); pick() render(1);");
    assert!(group_children(&group_children(&root)[0]).is_empty());
    assert!(has_warning(
        &evaluator,
        "Bad parameter type (undef) for children, only accept: empty, number, vector, range"
    ));
}

#[test]
fn children_duplicate_named_index_warns_and_last_wins() {
    // Parameters.cc parse_without_defaults: "argument %1$s supplied more
    // than once"; set_variable insert-or-assigns, so the last value wins.
    let (root, evaluator) =
        eval("module pick() children(index = 1, index = 0); pick() { render(1); fill(2); }");
    assert!(has_warning(
        &evaluator,
        "argument 'index' supplied more than once"
    ));
    let picked = group_children(&group_children(&group_children(&root)[0])[0]);
    assert_eq!(picked.len(), 1);
    assert_eq!(call_name(&picked[0]), "render");
}

#[test]
fn dollar_children_counts_module_instantiations() {
    // UserModuleContext binds $children to the children scope's module
    // instantiation count: assignments don't count, disabled (`*`) children
    // never exist (parser.y deletes them), and $children is lexical
    // (ContextFrame::is_config_variable special-cases it).
    let (_, evaluator) =
        eval("module m() echo($children); m() { a = 1; render(a); *fill(2); hull(3); }");
    assert_eq!(evaluator.console(), ["ECHO: 2"]);
}

#[test]
fn dollar_children_is_zero_without_children() {
    let (_, evaluator) = eval("module m() echo($children); m();");
    assert_eq!(evaluator.console(), ["ECHO: 0"]);
}

#[test]
fn children_evaluate_in_caller_scope_not_module_scope() {
    // Children.cc: the instantiation's children evaluate against the
    // *caller's* context (captured at instantiation), so a module-body
    // binding must not leak into them.
    let (root, evaluator) = eval("x = 2; module m() { hidden = 9; children(); } m() render(x);");
    let picked = group_children(&group_children(&group_children(&root)[0])[0]);
    let CsgNode::Call { args, .. } = &picked[0] else {
        panic!("expected a call node");
    };
    assert_eq!(args[0].1, n(2.0));
    assert!(evaluator.warnings().is_empty());

    let (_, evaluator) = eval("module m() { hidden = 9; children(); } m() render(hidden);");
    assert!(has_warning(
        &evaluator,
        "Ignoring unknown variable 'hidden'"
    ));
}

#[test]
fn dollar_variables_flow_from_module_body_into_children() {
    // Special variables resolve along the live dynamic chain (upstream
    // walks the session's frame stack), so a $-binding in the module body
    // is visible while its children evaluate.
    let (root, evaluator) = eval("module m() { $fn = 32; children(); } m() render($fn);");
    let picked = group_children(&group_children(&group_children(&root)[0])[0]);
    let CsgNode::Call { args, .. } = &picked[0] else {
        panic!("expected a call node");
    };
    assert_eq!(args[0].1, n(32.0));
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn children_scope_assignments_bind_for_selected_children() {
    // Children::instantiate creates a ScopeContext over the children scope,
    // so its assignments evaluate (in the caller's context) even when only
    // some children are selected.
    let (root, evaluator) =
        eval("module pick() children(0); pick() { a = 6; render(a); fill(2); }");
    let picked = group_children(&group_children(&group_children(&root)[0])[0]);
    let CsgNode::Call { args, .. } = &picked[0] else {
        panic!("expected a call node");
    };
    assert_eq!(args[0].1, n(6.0));
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn nested_modules_each_get_their_own_children() {
    let (root, evaluator) = eval(
        "module inner() children(); \
         module outer() inner() children(); \
         outer() render(1);",
    );
    // outer group > inner group > children() group > children() group > union
    let outer = group_children(&group_children(&root)[0]);
    let inner = group_children(&outer[0]);
    let inner_children = group_children(&inner[0]);
    let outer_children = group_children(&inner_children[0]);
    assert_eq!(call_name(&outer_children[0]), "render");
    assert!(evaluator.warnings().is_empty());
}

// -------------------------------------------------------------- modifiers

#[test]
fn disable_modifier_skips_evaluation_entirely() {
    // parser.y: '*' module_instantiation { delete $2; $$ = NULL; } — the
    // instantiation never evaluates, so no argument side effects, no echo
    // output, and no unknown-module warning.
    let (root, evaluator) = eval("*render(nope); *mystery(1); *echo(\"x\"); *m();");
    assert!(group_children(&root).is_empty());
    assert!(evaluator.warnings().is_empty());
    assert!(evaluator.console().is_empty());
}

#[test]
fn modifiers_are_recorded_on_nodes() {
    let (root, _) = eval("#render(1); !%fill(2); module m() render(1); #m();");
    let children = group_children(&root);
    let CsgNode::Call { modifiers, .. } = &children[0] else {
        panic!("expected a call node");
    };
    assert_eq!(
        *modifiers,
        Modifiers {
            debug: true,
            ..Modifiers::default()
        }
    );
    let CsgNode::Call { modifiers, .. } = &children[1] else {
        panic!("expected a call node");
    };
    assert!(modifiers.root && modifiers.background);
    let CsgNode::Group { modifiers, .. } = &children[2] else {
        panic!("expected a group node");
    };
    assert!(modifiers.debug);
}

// ------------------------------------------------------------------- echo

#[test]
fn echo_formats_arguments_to_the_console() {
    // control.cc builtin_echo logs STR(arguments); Arguments' stream
    // operator prints values via toEchoString (strings quoted).
    let (_, evaluator) = eval("echo(1, \"a\", [1, 2]);");
    assert_eq!(evaluator.console(), ["ECHO: 1, \"a\", [1, 2]"]);
}

#[test]
fn echo_prints_named_arguments_with_names() {
    let (_, evaluator) = eval("echo(a = 1, $fn = 2, true);");
    assert_eq!(evaluator.console(), ["ECHO: a = 1, $fn = 2, true"]);
}

#[test]
fn echo_without_children_produces_no_node() {
    let (root, _) = eval("echo(1);");
    assert!(group_children(&root).is_empty());
}

#[test]
fn echo_with_children_wraps_them_in_a_group() {
    let (root, evaluator) = eval("echo(\"making\") render(1);");
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    let wrapped = group_children(&children[0]);
    assert_eq!(call_name(&wrapped[0]), "render");
    assert_eq!(evaluator.console(), ["ECHO: \"making\""]);
}

// ------------------------------------------------------------------ assert

#[test]
fn assert_true_passes_without_error_or_node() {
    let (root, evaluator) = eval("assert(true);");
    assert!(group_children(&root).is_empty());
    assert!(evaluator.errors().is_empty());
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn assert_true_passes_children_through() {
    let (root, evaluator) = eval("assert(1 < 2) render(1);");
    let wrapped = group_children(&group_children(&root)[0]);
    assert_eq!(call_name(&wrapped[0]), "render");
    assert!(evaluator.errors().is_empty());
}

#[test]
fn assert_false_records_error_with_span() {
    // Assert::performAssert: a false condition is a hard error. Upstream
    // logs "Assertion%1$s failed%2$s" and throws AssertionFailedException;
    // here the error is recorded with the instantiation's span and only
    // this subtree is dropped.
    let src = "assert(false) render(1); fill(2);";
    let (root, evaluator) = eval(src);
    // The assert's subtree produces nothing; siblings still evaluate.
    let children = group_children(&root);
    assert_eq!(children.len(), 1);
    assert_eq!(call_name(&children[0]), "fill");
    assert_eq!(evaluator.errors().len(), 1);
    let error = &evaluator.errors()[0];
    assert!(error.message.contains("Assertion 'false' failed"));
    assert!(&src[error.span.start..error.span.end].starts_with("assert(false)"));
}

#[test]
fn assert_false_with_message_includes_echo_formatted_message() {
    // The message renders via toEchoString: strings are quoted.
    let (_, evaluator) = eval("assert(false, \"boom\");");
    assert_eq!(evaluator.errors().len(), 1);
    assert!(
        evaluator.errors()[0]
            .message
            .contains("Assertion 'false' failed: \"boom\""),
        "unexpected message: {}",
        evaluator.errors()[0].message
    );
}

#[test]
fn assert_message_includes_condition_text() {
    // Assert::performAssert logs "Assertion%1$s failed%2$s" where %1$s
    // quotes the condition expression — the first argument named
    // "condition" or unnamed; here its source text sliced by span.
    let (_, evaluator) = eval("x = 2; assert(x > 5, \"too small\");");
    assert_eq!(evaluator.errors().len(), 1);
    assert!(
        evaluator.errors()[0]
            .message
            .contains("Assertion 'x > 5' failed: \"too small\""),
        "unexpected message: {}",
        evaluator.errors()[0].message
    );

    // A named condition is quoted too.
    let (_, evaluator) = eval("assert(condition = 1 == 2);");
    assert!(
        evaluator.errors()[0]
            .message
            .contains("Assertion '1 == 2' failed"),
        "unexpected message: {}",
        evaluator.errors()[0].message
    );
}

#[test]
fn assert_missing_condition_fails() {
    // parameters["condition"] is undef without an argument; undef is falsy.
    let (_, evaluator) = eval("assert();");
    assert_eq!(evaluator.errors().len(), 1);
}

#[test]
fn echo_and_assert_work_inside_modules() {
    let (_, evaluator) = eval(
        "module m(v) { echo(\"v is\", v); assert(v > 0, \"v must be positive\"); } \
         m(3); m(-1);",
    );
    assert_eq!(
        evaluator.console(),
        ["ECHO: \"v is\", 3", "ECHO: \"v is\", -1"]
    );
    assert_eq!(evaluator.errors().len(), 1);
    assert!(evaluator.errors()[0]
        .message
        .contains("Assertion 'v > 0' failed: \"v must be positive\""));
}

// ------------------------------------------------------------ control flow

#[test]
fn statement_let_binds_a_child_scope_and_instantiates_children() {
    // Statement-`let` (upstream control.cc builtin_let) binds each named
    // argument in a fresh child scope, then instantiates its children in a
    // scope over that binding frame — the same pattern `for` uses for its
    // loop variable. `let(x=10) cube(x);` must therefore resolve to a real
    // cube primitive of the bound size, NOT an Unresolved placeholder.
    let (root, evaluator) = eval("let (x = 10) cube(x);");
    assert!(evaluator.errors().is_empty());
    let top = group_children(&root);
    assert_eq!(top.len(), 1);
    // The let instantiation itself wraps its children in a Group.
    let let_children = group_children(&top[0]);
    assert_eq!(let_children.len(), 1);
    let CsgNode::Primitive {
        primitive: Primitive::Cube { size, .. },
        ..
    } = &let_children[0]
    else {
        panic!("expected a cube primitive, got {:?}", let_children[0]);
    };
    assert_eq!(*size, [10.0, 10.0, 10.0]);
}

#[test]
fn statement_let_binds_args_left_to_right() {
    // Each let argument evaluates in the building child scope, so a later
    // argument sees prior bindings: `let(a=1, b=a+2)` binds b=3, and the
    // child cube(b) resolves to size 3. Matches OpenSCAD builtin_let, which
    // set_variable's each assignment into the same letctx in order.
    let (root, evaluator) = eval("let (a = 1, b = a + 2) cube(b);");
    assert!(evaluator.errors().is_empty());
    let top = group_children(&root);
    assert_eq!(top.len(), 1);
    let let_children = group_children(&top[0]);
    assert_eq!(let_children.len(), 1);
    let CsgNode::Primitive {
        primitive: Primitive::Cube { size, .. },
        ..
    } = &let_children[0]
    else {
        panic!("expected a cube primitive, got {:?}", let_children[0]);
    };
    assert_eq!(*size, [3.0, 3.0, 3.0]);
}

#[test]
fn statement_let_is_no_longer_an_unresolved_placeholder() {
    // Regression guard: the statement-let arm must never regress to the old
    // Unresolved placeholder, which produced no geometry.
    let (root, _) = eval("let (x = 1) cube(x);");
    let top = group_children(&root);
    assert!(
        !matches!(&top[0], CsgNode::Unresolved { kind, .. } if kind == "let"),
        "statement let must not regress to an Unresolved placeholder"
    );
}

#[test]
fn control_flow_counts_as_a_child_instantiation() {
    // An if/else is itself a module instantiation upstream, so it counts
    // toward $children.
    let (_, evaluator) = eval("module m() echo($children); m() { if (true) render(1); fill(2); }");
    assert_eq!(evaluator.console(), ["ECHO: 2"]);
}
