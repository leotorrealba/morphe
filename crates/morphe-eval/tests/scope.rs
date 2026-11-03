//! Lexical scoping with assignment-is-last-wins semantics.
//!
//! Every rule here is transcribed from upstream openscad
//! src/core/ContextFrame.{h,cc}, Context.{h,cc}, ScopeContext.cc,
//! EvaluationSession.cc, BuiltinContext.cc and the parser's
//! handle_assignment (parser.y) (master, fetched 2026-07-02).

use morphe_eval::scope::{merge_assignments, ScopeStack};
use morphe_eval::value::Value;

fn n(x: f64) -> Value {
    Value::Number(x)
}

/// Stand-in for an assignment's right-hand-side AST node.
#[derive(Clone, Copy)]
enum Rhs {
    Lit(f64),
    Var(&'static str),
}

/// Lower and bind one scope body, mimicking the evaluator's intended call
/// pattern: merge duplicates once, route the merge warnings, then evaluate
/// the merged list in order via the index-based callback.
fn bind(stack: &mut ScopeStack, body: Vec<(&'static str, Rhs)>) {
    let (merged, warnings) = merge_assignments(body);
    for w in warnings {
        stack.warn(w);
    }
    let names: Vec<&str> = merged.iter().map(|(name, _)| *name).collect();
    stack.bind_assignments(&names, |scope, i| match merged[i].1 {
        Rhs::Lit(x) => Value::Number(x),
        Rhs::Var(name) => scope.lookup(name),
    });
}

fn has_warning(stack: &ScopeStack, needle: &str) -> bool {
    stack.warnings().iter().any(|w| w.contains(needle))
}

// ---------------------------------------------------------------------------
// Builtins — upstream BuiltinContext::init (BuiltinContext.cc)
// ---------------------------------------------------------------------------

#[test]
fn pi_is_available_at_root() {
    let mut stack = ScopeStack::new();
    assert_eq!(stack.lookup("PI"), n(std::f64::consts::PI));
    assert!(stack.warnings().is_empty());
}

#[test]
fn pi_is_visible_from_nested_scopes() {
    let mut stack = ScopeStack::new();
    stack.push_scope();
    stack.push_scope();
    assert_eq!(stack.lookup("PI"), n(std::f64::consts::PI));
    assert!(stack.warnings().is_empty());
}

// ---------------------------------------------------------------------------
// Unknown variables — upstream Context::lookup_variable (Context.cc):
// warn "Ignoring unknown variable ..." and return Value::undefined.
// ---------------------------------------------------------------------------

#[test]
fn unknown_variable_is_undef_with_warning() {
    let mut stack = ScopeStack::new();
    let v = stack.lookup("nope");
    assert!(v.is_undef());
    // Upstream returns the plain `Value::undefined` singleton — no reason.
    assert_eq!(v.undef_message(), None);
    assert!(has_warning(&stack, "unknown variable 'nope'"));
}

#[test]
fn unknown_dollar_variable_is_undef_with_warning() {
    // Upstream EvaluationSession::lookup_special_variable warns identically.
    let mut stack = ScopeStack::new();
    assert!(stack.lookup("$nope").is_undef());
    assert!(has_warning(&stack, "unknown variable '$nope'"));
}

// ---------------------------------------------------------------------------
// Basic binding and the lexical chain — upstream Context::try_lookup_variable
// walks the parent chain; ContextFrame::set_variable binds this frame only.
// ---------------------------------------------------------------------------

#[test]
fn set_then_lookup_in_same_frame() {
    let mut stack = ScopeStack::new();
    stack.set("a", n(1.5));
    assert_eq!(stack.lookup("a"), n(1.5));
    assert!(stack.warnings().is_empty());
}

#[test]
fn lookup_walks_lexical_chain_outward() {
    let mut stack = ScopeStack::new();
    stack.set("a", n(1.0));
    stack.push_scope();
    stack.push_scope();
    assert_eq!(stack.lookup("a"), n(1.0));
    assert!(stack.warnings().is_empty());
}

#[test]
fn scopes_shadow_correctly() {
    let mut stack = ScopeStack::new();
    stack.set("a", n(1.0));
    stack.push_scope();
    stack.set("a", n(2.0));
    assert_eq!(stack.lookup("a"), n(2.0));
    stack.pop();
    assert_eq!(stack.lookup("a"), n(1.0));
    assert!(stack.warnings().is_empty());
}

#[test]
fn inner_assignment_never_mutates_outer_frame() {
    // OpenSCAD has no assignment-to-outer: set_variable always binds in the
    // current frame (ContextFrame::set_variable).
    let mut stack = ScopeStack::new();
    stack.set("a", n(1.0));
    stack.push_scope();
    bind(&mut stack, vec![("a", Rhs::Lit(99.0))]);
    assert_eq!(stack.lookup("a"), n(99.0));
    stack.pop();
    assert_eq!(stack.lookup("a"), n(1.0));
}

#[test]
fn sibling_scopes_are_isolated() {
    let mut stack = ScopeStack::new();
    stack.push_scope();
    stack.set("b", n(1.0));
    stack.pop();
    stack.push_scope();
    assert!(stack.lookup("b").is_undef());
    assert!(has_warning(&stack, "unknown variable 'b'"));
}

#[test]
fn pop_at_root_is_a_noop() {
    let mut stack = ScopeStack::new();
    stack.pop();
    stack.pop();
    assert_eq!(stack.lookup("PI"), n(std::f64::consts::PI));
}

// ---------------------------------------------------------------------------
// Frame arena discipline
// ---------------------------------------------------------------------------

#[test]
fn pop_reclaims_tail_frames() {
    // LIFO pops truncate the arena, so a fresh push reuses the same slot —
    // balanced push/pop cycles do not grow the arena.
    let mut stack = ScopeStack::new();
    stack.push_scope();
    let first = stack.current();
    stack.pop();
    stack.push_scope();
    assert_eq!(stack.current(), first);
}

#[test]
#[should_panic(expected = "not a live frame")]
fn push_call_rejects_dead_frame_id() {
    let mut stack = ScopeStack::new();
    stack.push_scope();
    let dead = stack.current();
    stack.pop(); // reclaims the tail frame; `dead` no longer names a frame
    stack.push_call(dead);
}

// ---------------------------------------------------------------------------
// Assignment-is-last-wins — upstream parser.y handle_assignment merges a
// re-assignment into the FIRST assignment's list position (replacing its
// expression, warning), then ScopeContext::init evaluates the merged list
// eagerly in order.
// ---------------------------------------------------------------------------

#[test]
fn merge_assignments_replaces_in_first_position_and_warns() {
    // Pure merge unit: b = ?; a = 1; a = 2  =>  [b = ?, a = 2] + one warning.
    let (merged, warnings) = merge_assignments(vec![("b", 0), ("a", 1), ("a", 2)]);
    assert_eq!(merged, vec![("b", 0), ("a", 2)]);
    assert_eq!(warnings, vec!["variable a was assigned twice".to_string()]);
}

#[test]
fn last_assignment_wins_within_scope() {
    let mut stack = ScopeStack::new();
    stack.push_scope();
    bind(&mut stack, vec![("a", Rhs::Lit(1.0)), ("a", Rhs::Lit(2.0))]);
    assert_eq!(stack.lookup("a"), n(2.0));
}

#[test]
fn duplicate_assignment_records_warning() {
    let mut stack = ScopeStack::new();
    stack.push_scope();
    bind(&mut stack, vec![("a", Rhs::Lit(1.0)), ("a", Rhs::Lit(2.0))]);
    assert!(has_warning(&stack, "variable a was assigned twice"));
}

#[test]
fn use_before_textual_assignment_sees_final_value() {
    // a = 1; b = a; a = 2;  =>  b == 2.
    // The override lands at `a`'s first position, so a=2 evaluates BEFORE b.
    let mut stack = ScopeStack::new();
    stack.push_scope();
    bind(
        &mut stack,
        vec![
            ("a", Rhs::Lit(1.0)),
            ("b", Rhs::Var("a")),
            ("a", Rhs::Lit(2.0)),
        ],
    );
    assert_eq!(stack.lookup("a"), n(2.0));
    assert_eq!(stack.lookup("b"), n(2.0));
    assert!(has_warning(&stack, "assigned twice"));
}

#[test]
fn assignments_evaluate_sequentially_seeing_earlier_bindings() {
    // a = 1; b = a; c = b;  =>  c == 1 (ScopeContext::init is sequential).
    let mut stack = ScopeStack::new();
    stack.push_scope();
    bind(
        &mut stack,
        vec![
            ("a", Rhs::Lit(1.0)),
            ("b", Rhs::Var("a")),
            ("c", Rhs::Var("b")),
        ],
    );
    assert_eq!(stack.lookup("c"), n(1.0));
    assert!(stack.warnings().is_empty());
}

#[test]
fn forward_reference_to_later_assignment_is_undef() {
    // b = a; a = 5;  =>  b == undef (upstream init is eager and sequential,
    // NOT a whole-scope fixpoint: `a` is not yet bound when `b` evaluates).
    let mut stack = ScopeStack::new();
    stack.push_scope();
    bind(&mut stack, vec![("b", Rhs::Var("a")), ("a", Rhs::Lit(5.0))]);
    assert!(stack.lookup("b").is_undef());
    assert_eq!(stack.lookup("a"), n(5.0));
    assert!(has_warning(&stack, "unknown variable 'a'"));
}

#[test]
fn override_keeps_first_assignment_position() {
    // b = a; a = 1; a = 2;  =>  merged list is [b = a, a = 2]: the override
    // replaces the expression in place, so `a` still evaluates AFTER `b`.
    let mut stack = ScopeStack::new();
    stack.push_scope();
    bind(
        &mut stack,
        vec![
            ("b", Rhs::Var("a")),
            ("a", Rhs::Lit(1.0)),
            ("a", Rhs::Lit(2.0)),
        ],
    );
    assert!(stack.lookup("b").is_undef());
    assert_eq!(stack.lookup("a"), n(2.0));
    assert!(has_warning(&stack, "variable a was assigned twice"));
}

#[test]
fn forward_reference_falls_back_to_outer_scope() {
    // Outer: a = 1. Inner: b = a; a = 2;  =>  b == 1 — while the inner scope
    // initializes, `a` is not yet bound locally, so the lookup walks outward.
    let mut stack = ScopeStack::new();
    stack.set("a", n(1.0));
    stack.push_scope();
    bind(&mut stack, vec![("b", Rhs::Var("a")), ("a", Rhs::Lit(2.0))]);
    assert_eq!(stack.lookup("b"), n(1.0));
    assert_eq!(stack.lookup("a"), n(2.0));
    stack.pop();
    assert_eq!(stack.lookup("a"), n(1.0));
    assert!(stack.warnings().is_empty());
}

#[test]
fn bindings_target_the_frame_active_at_bind_start() {
    // A misbehaving callback that pushes a frame without popping must not
    // retarget the remaining bindings.
    let mut stack = ScopeStack::new();
    stack.push_scope();
    let target = stack.current();
    stack.bind_assignments(&["a", "b"], |scope, i| {
        if i == 0 {
            scope.push_scope(); // stray frame, never popped
        }
        Value::Number(i as f64)
    });
    // Leave the stray frame; both bindings live in `target`.
    while stack.current() != target {
        stack.pop();
    }
    assert_eq!(stack.lookup("a"), n(0.0));
    assert_eq!(stack.lookup("b"), n(1.0));
    // ...and only in `target`: its parent has neither.
    stack.pop();
    assert!(stack.lookup("a").is_undef());
    assert!(stack.lookup("b").is_undef());
}

// ---------------------------------------------------------------------------
// Circular / self references — no cycle detection needed upstream: eager
// sequential initialization means the referenced name is simply not bound
// yet, so it resolves outward or to undef with an unknown-variable warning.
// ---------------------------------------------------------------------------

#[test]
fn self_reference_without_outer_binding_is_undef() {
    let mut stack = ScopeStack::new();
    stack.push_scope();
    bind(&mut stack, vec![("a", Rhs::Var("a"))]);
    assert!(stack.lookup("a").is_undef());
    assert!(has_warning(&stack, "unknown variable 'a'"));
}

#[test]
fn self_reference_sees_outer_binding() {
    // Outer: a = 1. Inner: a = a;  =>  inner a == 1 (outer value, not undef).
    let mut stack = ScopeStack::new();
    stack.set("a", n(1.0));
    stack.push_scope();
    bind(&mut stack, vec![("a", Rhs::Var("a"))]);
    assert_eq!(stack.lookup("a"), n(1.0));
    assert!(stack.warnings().is_empty());
}

#[test]
fn circular_references_yield_undef_with_warning() {
    // a = b; b = a;  =>  both undef, terminates without infinite regress.
    let mut stack = ScopeStack::new();
    stack.push_scope();
    bind(&mut stack, vec![("a", Rhs::Var("b")), ("b", Rhs::Var("a"))]);
    assert!(stack.lookup("a").is_undef());
    assert!(stack.lookup("b").is_undef());
    assert!(has_warning(&stack, "unknown variable 'b'"));
}

// ---------------------------------------------------------------------------
// $ special variables — upstream ContextFrame keeps separate
// lexical/config maps; is_config_variable(name) == (name[0] == '$' &&
// name != "$children"); config lookups walk the DYNAMIC call stack
// (EvaluationSession::try_lookup_special_variable), not the lexical chain.
// ---------------------------------------------------------------------------

#[test]
fn dollar_variable_set_and_lookup() {
    let mut stack = ScopeStack::new();
    stack.set("$fn", n(36.0));
    assert_eq!(stack.lookup("$fn"), n(36.0));
    assert!(stack.warnings().is_empty());
}

#[test]
fn dollar_lookup_is_dynamic_while_plain_lookup_is_lexical() {
    let mut stack = ScopeStack::new();
    let def_site = stack.current(); // "function" defined at root
    stack.set("g", n(42.0));

    stack.push_scope(); // caller scope
    stack.set("$fn", n(7.0));
    stack.set("x", n(7.0));

    // Call the function defined at root.
    stack.push_call(def_site);
    // $fn: dynamic chain (call frame -> caller -> root) finds the caller's 7.
    assert_eq!(stack.lookup("$fn"), n(7.0));
    // g: lexical chain (call frame -> root) finds root's 42.
    assert_eq!(stack.lookup("g"), n(42.0));
    // x: lexical chain skips the caller frame entirely.
    assert!(stack.lookup("x").is_undef());
    assert!(has_warning(&stack, "unknown variable 'x'"));
}

#[test]
fn dollar_inner_frame_shadows_outer_dynamically() {
    let mut stack = ScopeStack::new();
    stack.push_scope();
    stack.set("$fa", n(12.0));
    stack.push_scope();
    stack.set("$fa", n(6.0));
    assert_eq!(stack.lookup("$fa"), n(6.0));
    stack.pop();
    assert_eq!(stack.lookup("$fa"), n(12.0));
}

#[test]
fn dollar_binding_does_not_mutate_outer_frame() {
    let mut stack = ScopeStack::new();
    stack.set("$t", n(0.0));
    stack.push_scope();
    bind(&mut stack, vec![("$t", Rhs::Lit(0.5))]);
    assert_eq!(stack.lookup("$t"), n(0.5));
    stack.pop();
    assert_eq!(stack.lookup("$t"), n(0.0));
}

#[test]
fn children_is_lexical_not_dynamic() {
    // Upstream ContextFrame::is_config_variable exempts "$children": it is an
    // ordinary lexical binding despite the '$' prefix.
    let mut stack = ScopeStack::new();
    let def_site = stack.current();
    stack.push_scope(); // caller scope
    stack.set("$children", n(3.0));
    stack.set("$fn", n(9.0));
    stack.push_call(def_site);
    assert_eq!(stack.lookup("$fn"), n(9.0)); // dynamic: caller frame visible
    assert!(stack.lookup("$children").is_undef()); // lexical: caller skipped
    assert!(has_warning(&stack, "unknown variable '$children'"));
}

#[test]
fn bare_dollar_sigil_is_lexical() {
    // Defensive: a bare "$" (never produced by the lexer) is not a config
    // variable — it follows the lexical chain like a plain name.
    let mut stack = ScopeStack::new();
    let def_site = stack.current();
    stack.push_scope(); // caller scope
    stack.set("$", n(1.0));
    stack.push_call(def_site);
    assert!(stack.lookup("$").is_undef()); // lexical chain skips the caller
}

#[test]
fn dollar_assignments_also_last_wins() {
    // parser.y handle_assignment does not special-case $-names.
    let mut stack = ScopeStack::new();
    stack.push_scope();
    bind(
        &mut stack,
        vec![("$fs", Rhs::Lit(1.0)), ("$fs", Rhs::Lit(2.0))],
    );
    assert_eq!(stack.lookup("$fs"), n(2.0));
    assert!(has_warning(&stack, "variable $fs was assigned twice"));
}

// ---------------------------------------------------------------------------
// Evaluator integration shape — the scope API must borrow cleanly when the
// ScopeStack is a field of a larger evaluator that also owns the AST and
// evaluates via a free function (no E0502).
// ---------------------------------------------------------------------------

enum MiniExpr {
    Num(f64),
    Ref(String),
    Add(Box<MiniExpr>, Box<MiniExpr>),
}

fn eval_expr(expr: &MiniExpr, scope: &mut ScopeStack) -> Value {
    match expr {
        MiniExpr::Num(x) => Value::Number(*x),
        MiniExpr::Ref(name) => scope.lookup(name),
        MiniExpr::Add(a, b) => eval_expr(a, scope).add(&eval_expr(b, scope)),
    }
}

struct MiniEvaluator {
    scope: ScopeStack,
    body: Vec<(String, MiniExpr)>,
}

impl MiniEvaluator {
    fn bind_body(&mut self) {
        let (merged, warnings) =
            merge_assignments(self.body.iter().map(|(n, e)| (n.as_str(), e)).collect());
        for w in warnings {
            self.scope.warn(w);
        }
        let names: Vec<&str> = merged.iter().map(|(name, _)| *name).collect();
        self.scope
            .bind_assignments(&names, |scope, i| eval_expr(merged[i].1, scope));
    }
}

#[test]
fn evaluator_shape_borrows_cleanly() {
    // a = 1; b = a + PI; a = 2;  =>  a == 2, b == 2 + PI.
    let mut evaluator = MiniEvaluator {
        scope: ScopeStack::new(),
        body: vec![
            ("a".to_string(), MiniExpr::Num(1.0)),
            (
                "b".to_string(),
                MiniExpr::Add(
                    Box::new(MiniExpr::Ref("a".to_string())),
                    Box::new(MiniExpr::Ref("PI".to_string())),
                ),
            ),
            ("a".to_string(), MiniExpr::Num(2.0)),
        ],
    };
    evaluator.scope.push_scope();
    evaluator.bind_body();
    assert_eq!(evaluator.scope.lookup("a"), n(2.0));
    assert_eq!(evaluator.scope.lookup("b"), n(2.0 + std::f64::consts::PI));
    assert!(has_warning(
        &evaluator.scope,
        "variable a was assigned twice"
    ));
}

// ------------------------------------------------------- retained frames

#[test]
fn unretained_frame_slot_is_reused_after_pop() {
    // Baseline arena behavior: balanced push/pop reclaims the slot.
    let mut stack = ScopeStack::new();
    stack.push_scope();
    let id = stack.current();
    stack.pop();
    stack.push_scope();
    assert_eq!(stack.current(), id);
}

#[test]
fn retained_frame_id_is_never_reused_after_pop() {
    // A definition site pinned with retain() survives its pop: the id is
    // not reused by a later push and stays addressable for push_call.
    let mut stack = ScopeStack::new();
    stack.push_scope();
    let def_site = stack.current();
    stack.set("captured", n(7.0));
    stack.retain(def_site);
    stack.pop();

    stack.push_scope();
    assert_ne!(stack.current(), def_site);
    stack.set("captured", n(9.0));

    stack.push_call(def_site);
    assert_eq!(stack.lookup("captured"), n(7.0));
}
