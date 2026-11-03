//! Lexical scoping with assignment-is-last-wins semantics.
//!
//! Semantics are ported from upstream openscad `src/core/ContextFrame.{h,cc}`,
//! `Context.{h,cc}`, `ScopeContext.cc`, `EvaluationSession.cc`,
//! `BuiltinContext.cc` and the parser's `handle_assignment` (`parser.y`)
//! (master, fetched 2026-07-02):
//!
//! - Each frame keeps separate lexical and config (`$`) maps (upstream
//!   `ContextFrame`). Plain names resolve along the *lexical* parent chain
//!   (`Context::try_lookup_variable`); `$`-names — except `$children`, per
//!   `ContextFrame::is_config_variable` — resolve along the *dynamic* call
//!   chain (`EvaluationSession::try_lookup_special_variable` walks the live
//!   frame stack top-down).
//! - Assignment always binds in the current frame
//!   (`ContextFrame::set_variable`); OpenSCAD has no assignment-to-outer, so
//!   outer frames are never mutated.
//! - Within one scope body, a re-assignment replaces the first assignment's
//!   expression *in place* — keeping the first occurrence's position in the
//!   list — and warns once, at parse/lowering time (`parser.y
//!   handle_assignment`; here: [`merge_assignments`]). The merged list then
//!   evaluates eagerly in order (`ScopeContext::init`; here:
//!   [`ScopeStack::bind_assignments`]), so the LAST expression wins but runs
//!   at the FIRST assignment's position: later references in the same scope
//!   see the final value, while references *before* a name's first
//!   assignment fall through to outer scopes or undef. Circular references
//!   therefore terminate naturally.
//! - Unknown lookups warn and yield plain undef (`Context::lookup_variable`
//!   returns `Value::undefined`).
//! - The root frame carries the builtin bindings: `PI` plus the special
//!   (`$`) variables `$fn`/`$fa`/`$fs`/`$t`/`$preview`/`$vp*`/`$parent_modules`
//!   (`Builtins.cc`; seeded in [`ScopeStack::new`]).

use std::collections::HashMap;

use crate::value::Value;

/// Handle to a frame in a [`ScopeStack`] arena (e.g. a definition site
/// captured for a later [`ScopeStack::push_call`]).
///
/// A `FrameId` is valid while its frame is alive: frames popped in LIFO
/// order are reclaimed (see [`ScopeStack::pop`]), so do not retain an id
/// across the pop of its frame.
pub type FrameId = usize;

/// Upstream `ContextFrame::is_config_variable`: `$`-names are dynamically
/// scoped, except `$children`, which is an ordinary lexical binding.
///
/// The length guard is defensive: upstream checks `name[0] == '$'` only,
/// but its lexer (like ours) can never produce a bare `"$"` identifier.
pub(crate) fn is_config_variable(name: &str) -> bool {
    name.len() >= 2 && name.starts_with('$') && name != "$children"
}

/// One scope frame (upstream `ContextFrame`): separate lexical/config maps
/// plus its links into the lexical and dynamic chains.
#[derive(Debug, Default)]
struct Frame {
    lexical_parent: Option<FrameId>,
    dynamic_parent: Option<FrameId>,
    lexical_vars: HashMap<String, Value>,
    config_vars: HashMap<String, Value>,
    /// Pinned by [`ScopeStack::retain`]: never reclaimed on pop.
    retained: bool,
}

impl Frame {
    /// Upstream `ContextFrame::lookup_local_variable`.
    fn get(&self, name: &str) -> Option<&Value> {
        if is_config_variable(name) {
            self.config_vars.get(name)
        } else {
            self.lexical_vars.get(name)
        }
    }
}

/// Merge one scope body's assignments with last-wins semantics (upstream
/// `parser.y handle_assignment`, which upstream runs at parse time).
///
/// A later assignment to an already-listed name replaces the earlier
/// payload *in place* — keeping the first assignment's position — and
/// records a `"variable <name> was assigned twice"` warning. Run this once
/// per scope body while lowering the AST (`T` is typically an `&Expr`),
/// route the warnings through [`ScopeStack::warn`], then feed the merged
/// list to [`ScopeStack::bind_assignments`].
pub fn merge_assignments<T>(assignments: Vec<(&str, T)>) -> (Vec<(&str, T)>, Vec<String>) {
    let mut merged: Vec<(&str, T)> = Vec::with_capacity(assignments.len());
    let mut warnings = Vec::new();
    for (name, payload) in assignments {
        if let Some(slot) = merged.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = payload;
            warnings.push(format!("variable {name} was assigned twice"));
        } else {
            merged.push((name, payload));
        }
    }
    (merged, warnings)
}

/// Arena of scope frames with a current-frame cursor.
///
/// Frames popped in LIFO order (the common case) are reclaimed immediately,
/// so the arena does not grow across balanced push/pop cycles. A frame
/// popped while newer frames are still alive is kept until the arena tail
/// retreats past it. Frames that must outlive their pop — definition sites
/// captured by the evaluator's function/module tables — are pinned with
/// [`ScopeStack::retain`], which keeps their [`FrameId`] from ever being
/// reused by a later push.
#[derive(Debug)]
pub struct ScopeStack {
    frames: Vec<Frame>,
    current: FrameId,
    warnings: Vec<String>,
}

impl ScopeStack {
    /// The root (builtin) frame.
    pub const ROOT: FrameId = 0;

    /// New stack whose root frame holds the builtin bindings — upstream
    /// `BuiltinContext::init` sets `PI`, and `Builtins::Builtins()` seeds
    /// the special (`$`) variables (`src/core/Builtins.cc`, master, fetched
    /// 2026-07-03, lines 112-129). All the specials are config variables, so
    /// they are dynamically scoped: a value assigned deeper in the call
    /// chain shadows the seed only for that subtree. Anything unseeded
    /// resolves to undef.
    pub fn new() -> ScopeStack {
        let mut stack = ScopeStack {
            frames: vec![Frame::default()],
            current: Self::ROOT,
            warnings: Vec::new(),
        };
        stack.set("PI", Value::Number(std::f64::consts::PI));
        // Fragment resolution + animation time (Builtins.cc 112-119).
        stack.set("$fn", Value::Number(0.0));
        stack.set("$fs", Value::Number(2.0));
        stack.set("$fa", Value::Number(12.0));
        stack.set("$t", Value::Number(0.0));
        // `$preview` seeds to undef upstream ("undef as should always be
        // overwritten"): a headless evaluation runs no preview/render pass,
        // so it stays undef rather than a bool (Builtins.cc 120-121).
        stack.set("$preview", Value::Undef(None));
        // Viewport vars (Builtins.cc 122-129): translation/rotation seed to
        // the zero vector, distance to 500, field of view to 22.5.
        let zero_vector = Value::from(vec![
            Value::Number(0.0),
            Value::Number(0.0),
            Value::Number(0.0),
        ]);
        stack.set("$vpt", zero_vector.clone());
        stack.set("$vpr", zero_vector);
        stack.set("$vpd", Value::Number(500.0));
        stack.set("$vpf", Value::Number(22.5));
        // `$parent_modules` is not seeded by upstream `Builtins`; it is
        // bound per instantiation by `UserModuleContext` from the
        // module-name stack depth (`ScopeContext.cc`;
        // [`Evaluator::instantiate_user_module`]). Seed 0 at the root so a
        // top-level read is defined (0 enclosing modules) rather than
        // warning-undef.
        stack.set("$parent_modules", Value::Number(0.0));
        stack
    }

    /// The currently active frame.
    pub fn current(&self) -> FrameId {
        self.current
    }

    /// The lexical parent of `frame` (`None` at the root). Lets the
    /// evaluator walk the lexical chain for its per-frame definition
    /// tables (upstream `Context::lookup_module`/`lookup_function` walk
    /// the context chain the same way).
    ///
    /// # Panics
    ///
    /// Panics if `frame` is not a live frame of this stack.
    pub fn lexical_parent(&self, frame: FrameId) -> Option<FrameId> {
        self.frames[frame].lexical_parent
    }

    /// Resolve `name` in the current frame only, without walking any
    /// parent chain (upstream `ContextFrame::lookup_local_variable`).
    pub fn lookup_local(&self, name: &str) -> Option<&Value> {
        self.frames[self.current].get(name)
    }

    /// Resolve `name` in `frame` only, without walking any parent chain
    /// (upstream `ContextFrame::lookup_local_variable` on that frame).
    ///
    /// # Panics
    ///
    /// Panics if `frame` is not a live frame of this stack.
    pub fn lookup_in(&self, frame: FrameId, name: &str) -> Option<&Value> {
        self.frames[frame].get(name)
    }

    /// Enter a nested lexical scope (a block body): both the lexical and the
    /// dynamic parent are the current frame.
    pub fn push_scope(&mut self) {
        self.push(self.current);
    }

    /// Enter a call frame: the lexical parent is the callee's definition
    /// site, while the dynamic parent is the caller (upstream pushes the new
    /// context frame onto the session's special-variable stack).
    ///
    /// # Panics
    ///
    /// Panics if `lexical_parent` is not a live frame of this stack — that
    /// is a caller bug (e.g. a [`FrameId`] retained across the pop that
    /// reclaimed its frame).
    pub fn push_call(&mut self, lexical_parent: FrameId) {
        assert!(
            lexical_parent < self.frames.len(),
            "push_call: frame id {lexical_parent} is not a live frame"
        );
        self.push(lexical_parent);
    }

    /// Enter a frame with NO lexical parent (its dynamic parent is the
    /// current frame): plain-name lookups from inside it see only what is
    /// bound in it, while `$`-names still resolve along the call chain.
    ///
    /// Used for a `use`d file's top-level scope, which upstream evaluates
    /// in its own `FileContext` over the builtin context
    /// (`src/core/SourceFile.cc`, `FileContext.cc`) — the importing file's
    /// variables and definitions are not visible to it. The caller re-binds
    /// whatever builtins the file should see (`PI`).
    pub fn push_isolated(&mut self) {
        self.push_with(None);
    }

    fn push(&mut self, lexical_parent: FrameId) {
        self.push_with(Some(lexical_parent));
    }

    fn push_with(&mut self, lexical_parent: Option<FrameId>) {
        let id = self.frames.len();
        self.frames.push(Frame {
            lexical_parent,
            dynamic_parent: Some(self.current),
            ..Frame::default()
        });
        self.current = id;
    }

    /// Leave the current frame, returning to its dynamic parent. A no-op at
    /// the root. If the popped frame is the arena tail (LIFO discipline)
    /// and not [retained](ScopeStack::retain), it is reclaimed, keeping
    /// arena growth bounded.
    pub fn pop(&mut self) {
        if let Some(parent) = self.frames[self.current].dynamic_parent {
            if self.current == self.frames.len() - 1 && !self.frames[self.current].retained {
                self.frames.pop();
            }
            self.current = parent;
        }
    }

    /// Pop frames until `base` is current again. `base` must be on the
    /// current frame's dynamic chain (e.g. the frame that was current before
    /// a run of pushes); popping stops at the root otherwise.
    pub fn pop_to(&mut self, base: FrameId) {
        while self.current != base && self.frames[self.current].dynamic_parent.is_some() {
            self.pop();
        }
    }

    /// The config (`$`) bindings made in the frames between the current
    /// frame (inclusive) and `base` (exclusive) along the dynamic chain,
    /// innermost binding first-wins. Used to carry `$`-variables across a
    /// tail call that replaces those frames, the analogue of upstream
    /// `Context::apply_config_variables` (`src/core/Expression.cc`
    /// `simplify_function_body`).
    pub fn config_vars_above(&self, base: FrameId) -> Vec<(String, Value)> {
        let mut out: Vec<(String, Value)> = Vec::new();
        let mut id = self.current;
        while id != base {
            let frame = &self.frames[id];
            for (name, value) in &frame.config_vars {
                if !out.iter().any(|(n, _)| n == name) {
                    out.push((name.clone(), value.clone()));
                }
            }
            match frame.dynamic_parent {
                Some(parent) => id = parent,
                None => break,
            }
        }
        out
    }

    /// Pin `frame` so its slot survives its pop: the [`FrameId`] stays
    /// valid — and is never reused by a later push — for the lifetime of
    /// the stack. Used for definition sites captured by the evaluator's
    /// function/module tables, whose [`ScopeStack::push_call`] may come
    /// long after the defining scope was popped (module bodies depend on
    /// this).
    ///
    /// # Panics
    ///
    /// Panics if `frame` is not a live frame of this stack.
    pub fn retain(&mut self, frame: FrameId) {
        self.frames[frame].retained = true;
    }

    /// Number of frame slots in the arena — test-only observability for
    /// leak regressions (e.g. module instantiations must not pin frames).
    #[cfg(test)]
    pub(crate) fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Bind `name` in the *current* frame (upstream
    /// `ContextFrame::set_variable` — an insert-or-assign on this frame;
    /// there is no assignment to outer frames).
    pub fn set(&mut self, name: &str, value: Value) {
        self.set_in(self.current, name, value);
    }

    /// Bind `name` in an explicit frame (used by
    /// [`bind_assignments`](ScopeStack::bind_assignments) so that a
    /// frame-pushing callback cannot retarget the bindings).
    fn set_in(&mut self, frame: FrameId, name: &str, value: Value) {
        let frame = &mut self.frames[frame];
        if is_config_variable(name) {
            frame.config_vars.insert(name.to_string(), value);
        } else {
            frame.lexical_vars.insert(name.to_string(), value);
        }
    }

    /// Resolve `name` without warning (upstream
    /// `Context::try_lookup_variable` / `try_lookup_special_variable`):
    /// plain names walk the lexical chain, config (`$`) names the dynamic
    /// chain.
    pub fn try_lookup(&self, name: &str) -> Option<&Value> {
        let dynamic = is_config_variable(name);
        let mut frame = &self.frames[self.current];
        loop {
            if let Some(value) = frame.get(name) {
                return Some(value);
            }
            let parent = if dynamic {
                frame.dynamic_parent
            } else {
                frame.lexical_parent
            };
            match parent {
                Some(id) => frame = &self.frames[id],
                None => return None,
            }
        }
    }

    /// Resolve `name`; unknown names warn and yield plain undef (upstream
    /// `Context::lookup_variable`).
    pub fn lookup(&mut self, name: &str) -> Value {
        match self.try_lookup(name) {
            Some(value) => value.clone(),
            None => {
                self.warn(format!("Ignoring unknown variable '{name}'"));
                Value::Undef(None)
            }
        }
    }

    /// Evaluate and bind a scope body's *pre-merged* assignment list
    /// (upstream `ScopeContext::init`): for each `names[i]` in order, call
    /// `eval(self, i)` and bind the result in the frame that was current
    /// when the loop started — a callback that pushes frames without
    /// popping cannot retarget the bindings.
    ///
    /// Duplicates must already be merged via [`merge_assignments`]; this
    /// method records no duplicate warnings. The index-based callback lets
    /// an evaluator keep its AST outside the closure, so only the scope is
    /// borrowed mutably (an `Evaluator { scope: ScopeStack, .. }` calling a
    /// free `eval_expr(&Expr, &mut ScopeStack) -> Value` borrows cleanly):
    ///
    /// ```
    /// use morphe_eval::scope::{merge_assignments, ScopeStack};
    /// use morphe_eval::value::Value;
    ///
    /// // Lowering a scope body `a = 1; a = 2;` (f64 stands in for &Expr):
    /// let body = vec![("a", 1.0), ("a", 2.0)];
    /// let (merged, warnings) = merge_assignments(body);
    /// let mut scope = ScopeStack::new();
    /// scope.push_scope();
    /// for w in warnings {
    ///     scope.warn(w);
    /// }
    /// let names: Vec<&str> = merged.iter().map(|(n, _)| *n).collect();
    /// scope.bind_assignments(&names, |_scope, i| Value::Number(merged[i].1));
    /// assert_eq!(scope.lookup("a"), Value::Number(2.0));
    /// ```
    pub fn bind_assignments<F>(&mut self, names: &[&str], mut eval: F)
    where
        F: FnMut(&mut ScopeStack, usize) -> Value,
    {
        let target = self.current;
        for (i, name) in names.iter().enumerate() {
            let value = eval(self, i);
            self.set_in(target, name, value);
        }
    }

    /// Warnings recorded so far, in order.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Record an evaluation warning.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

impl Default for ScopeStack {
    fn default() -> Self {
        Self::new()
    }
}
