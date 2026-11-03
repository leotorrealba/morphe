//! OpenSCAD runtime value model.
//!
//! Semantics are ported from upstream openscad `src/core/Value.{h,cc}`,
//! `RangeType.h`, `UndefType.h` and `str_utf8_wrapper.h` (master,
//! fetched 2026-07-02). Every operation mirrors the upstream visitor:
//! unsupported type combinations yield `Value::Undef` carrying the
//! upstream reason string instead of erroring.

use std::fmt;
use std::sync::Arc;

use morphe_lang::ast::{Expr, Param};

/// A callable user function: a `function name(...) = ...;` definition or
/// the value of a function literal (upstream `FunctionType`, held by a
/// `FunctionPtr` in `src/core/Value.h`).
///
/// `def_frame` is the evaluator frame the function closes over — the
/// definition site, or for a literal the scope it was evaluated in
/// (upstream `FunctionType::getContext`). It is only meaningful to the
/// [`Evaluator`](crate::eval::Evaluator) that created the value, which pins
/// the frame for as long as it lives.
#[derive(Debug)]
pub struct FunctionValue {
    /// The definition's name, for diagnostics; empty for a literal.
    pub name: String,
    pub(crate) def_frame: usize,
    pub params: Arc<Vec<Param>>,
    pub body: Arc<Expr>,
}

impl fmt::Display for FunctionValue {
    /// Upstream `operator<<(std::ostream&, const FunctionType&)`:
    /// `function(` parameters (with ` = default`) `) ` body, the body
    /// printed by [`crate::print::expr_to_string`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("function(")?;
        let mut params = String::new();
        crate::print::write_params(&mut params, &self.params)?;
        f.write_str(&params)?;
        f.write_str(") ")?;
        f.write_str(&crate::print::expr_to_string(&self.body))
    }
}

/// OpenSCAD range `[start : step : end]` (upstream `RangeType`).
#[derive(Debug, Clone)]
pub struct Range {
    pub start: f64,
    pub step: f64,
    pub end: f64,
}

impl Range {
    pub fn new(start: f64, step: f64, end: f64) -> Self {
        Range { start, step, end }
    }

    /// Number of iteration values (upstream `RangeType::numValues`).
    ///
    /// Returns `u32::MAX` when the range is unbounded (zero step or an
    /// infinite bound); iteration nevertheless treats a zero step as empty,
    /// matching the upstream iterator constructor.
    pub fn num_values(&self) -> u32 {
        if self.start.is_nan() || self.end.is_nan() || self.step.is_nan() {
            return 0;
        }
        if self.step < 0.0 {
            if self.start < self.end {
                return 0;
            }
        } else if self.start > self.end {
            return 0;
        }
        if self.start == self.end || self.step.is_infinite() {
            return 1;
        }
        if self.start.is_infinite() || self.end.is_infinite() || self.step == 0.0 {
            return u32::MAX;
        }
        // Upstream uses nextafter toward max to compensate for FP results
        // landing just below a whole number.
        let steps = ((self.end - self.start) / self.step).next_up();
        let num_steps = steps as u32; // saturates, like upstream's guarded cast
        if num_steps == u32::MAX {
            u32::MAX
        } else {
            num_steps + 1
        }
    }

    /// Iterate the range values (upstream `RangeType::iterator`): the i-th
    /// value is `start + step * i`. Zero step or any NaN bound iterates empty.
    pub fn iter(&self) -> RangeIter {
        let n =
            if self.step == 0.0 || self.start.is_nan() || self.step.is_nan() || self.end.is_nan() {
                0
            } else {
                self.num_values()
            };
        RangeIter {
            start: self.start,
            step: self.step,
            i: 0,
            n,
        }
    }

    fn scad_lt(&self, other: &Range) -> bool {
        let n1 = self.num_values();
        let n2 = other.num_values();
        if n1 == 0 {
            return n2 > 0;
        }
        if n2 == 0 {
            return false;
        }
        self.start < other.start
            || (self.start == other.start
                && (self.step < other.step || (self.step == other.step && n1 < n2)))
    }

    fn scad_le(&self, other: &Range) -> bool {
        let n1 = self.num_values();
        let n2 = other.num_values();
        if n1 == 0 {
            return true;
        }
        if n2 == 0 {
            return false;
        }
        self.start < other.start
            || (self.start == other.start
                && (self.step < other.step || (self.step == other.step && n1 <= n2)))
    }

    fn scad_gt(&self, other: &Range) -> bool {
        let n1 = self.num_values();
        let n2 = other.num_values();
        if n2 == 0 {
            return n1 > 0;
        }
        if n1 == 0 {
            return false;
        }
        self.start > other.start
            || (self.start == other.start
                && (self.step > other.step || (self.step == other.step && n1 > n2)))
    }

    fn scad_ge(&self, other: &Range) -> bool {
        let n1 = self.num_values();
        let n2 = other.num_values();
        if n2 == 0 {
            return true;
        }
        if n1 == 0 {
            return false;
        }
        self.start > other.start
            || (self.start == other.start
                && (self.step > other.step || (self.step == other.step && n1 >= n2)))
    }
}

/// Upstream `RangeType::operator==`: value-count based — two empty ranges
/// compare equal regardless of their fields, and `end` only participates
/// through `num_values()`.
impl PartialEq for Range {
    fn eq(&self, other: &Self) -> bool {
        let n1 = self.num_values();
        let n2 = other.num_values();
        if n1 == 0 {
            return n2 == 0;
        }
        if n2 == 0 {
            return false;
        }
        self.start == other.start && self.step == other.step && n1 == n2
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{} : {} : {}]",
            format_number(self.start),
            format_number(self.step),
            format_number(self.end)
        )
    }
}

/// Iterator over [`Range`] values.
pub struct RangeIter {
    start: f64,
    step: f64,
    i: u32,
    n: u32,
}

impl Iterator for RangeIter {
    type Item = f64;

    fn next(&mut self) -> Option<f64> {
        if self.i >= self.n {
            return None;
        }
        let v = self.start + self.step * f64::from(self.i);
        self.i += 1;
        Some(v)
    }
}

/// OpenSCAD runtime value (upstream `Value`).
///
/// `Undef` carries the optional upstream "reason" string (`UndefType`
/// reasons, joined with `\n\t`); equality ignores it.
///
/// Strings and vectors are immutable and reference-counted, like upstream's
/// `str_utf8_wrapper` and `VectorType` (both wrap a `shared_ptr`): cloning a
/// `Value` — every variable lookup, argument binding and vector element read
/// — is O(1) and shares the payload, so `let(a = big) [a, a]` stores `big`
/// once. Values are never mutated in place; operations build new values.
/// `Arc` (not `Rc`) keeps `Value: Send`, so evaluation can run on a worker
/// thread ([`crate::stack::with_large_stack`]).
#[derive(Debug, Clone)]
pub enum Value {
    Undef(Option<Box<str>>),
    Bool(bool),
    Number(f64),
    String(Arc<str>),
    Vector(Arc<Vec<Value>>),
    Range(Range),
    /// A function value (upstream `Value::Type::FUNCTION`).
    Function(Arc<FunctionValue>),
}

// Evaluation runs on a dedicated worker thread, so values must stay
// thread-safe; fail the build if a non-`Send` payload sneaks in.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Value>();
};

/// Tear nested vectors down iteratively.
///
/// The derived drop glue recurses once per nesting level, and tail calls
/// make million-deep nests easy to build (`f(n - 1, [acc])` accumulating
/// into a linked list), which would overflow the native stack when the
/// value is released — possibly on the embedder's small-stack thread, long
/// after evaluation. Instead, the last owner of a vector moves its elements
/// onto a heap worklist, and each element is released only once its own
/// uniquely owned children have been moved off, so every nested drop is
/// shallow. Shared vectors just drop a reference, as before.
impl Drop for Value {
    fn drop(&mut self) {
        let Value::Vector(items) = self else {
            return;
        };
        let Some(items) = Arc::get_mut(items) else {
            return;
        };
        if !items.iter().any(|item| matches!(item, Value::Vector(_))) {
            return; // flat: the default drop recurses at most one level
        }
        let mut pending = std::mem::take(items);
        while let Some(mut value) = pending.pop() {
            if let Value::Vector(inner) = &mut value {
                if let Some(inner) = Arc::get_mut(inner) {
                    pending.append(inner);
                }
            }
            // `value` drops here with no uniquely owned children left.
        }
    }
}

impl From<Vec<Value>> for Value {
    /// Wrap owned elements as a vector value.
    fn from(items: Vec<Value>) -> Value {
        Value::Vector(Arc::new(items))
    }
}

// Comparison/arithmetic methods intentionally shadow std trait names: they
// return `Value` (possibly undef) per OpenSCAD coercion rules, not
// bool/Self as the std traits require.
#[allow(clippy::should_implement_trait)]
impl Value {
    /// Undef with a reason, as upstream `Value::undef(msg)`.
    pub fn undef(msg: impl Into<String>) -> Value {
        Value::Undef(Some(msg.into().into_boxed_str()))
    }

    pub fn is_undef(&self) -> bool {
        matches!(self, Value::Undef(_))
    }

    pub fn undef_message(&self) -> Option<&str> {
        match self {
            Value::Undef(m) => m.as_deref(),
            _ => None,
        }
    }

    /// Upstream `Value::toEchoString()`: like `Display`, but strings are
    /// quoted at the top level too (used by `echo` and `assert` messages).
    pub fn to_echo_string(&self) -> String {
        match self {
            Value::String(s) => format!("\"{s}\""),
            other => other.to_string(),
        }
    }

    /// Upstream `Value::typeName()`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Undef(_) => "undefined",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Vector(_) => "vector",
            Value::Range(_) => "range",
            Value::Function(_) => "function",
        }
    }

    /// Truthiness, upstream `Value::toBool()`. Note NaN is truthy
    /// (`NaN != 0`), and ranges are always truthy even when they iterate
    /// empty.
    pub fn to_bool(&self) -> bool {
        match self {
            Value::Undef(_) => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0,
            Value::String(s) => !s.is_empty(),
            Value::Vector(v) => !v.is_empty(),
            // Upstream tobool_visitor: a function is always true.
            Value::Range(_) | Value::Function(_) => true,
        }
    }

    /// `<` — upstream `less_visitor`: mismatched types yield undef.
    pub fn lt(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Bool(a < b),
            (Value::String(a), Value::String(b)) => Value::Bool(a < b),
            (Value::Bool(a), Value::Bool(b)) => Value::Bool(a < b),
            (Value::Vector(a), Value::Vector(b)) => vec_lt(a, b),
            (Value::Range(a), Value::Range(b)) => Value::Bool(a.scad_lt(b)),
            (Value::Undef(_), Value::Undef(_)) => {
                Value::undef("operation undefined (undefined < undefined)")
            }
            _ => Value::undef(format!(
                "undefined operation ({} < {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// `<=` — upstream `lessequal_visitor`.
    pub fn le(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Bool(a <= b),
            (Value::String(a), Value::String(b)) => Value::Bool(a <= b),
            (Value::Bool(a), Value::Bool(b)) => Value::Bool(a <= b),
            // Upstream VectorType::operator<= is !(self > other).
            (Value::Vector(a), Value::Vector(b)) => negate_comparison(vec_lt(b, a)),
            (Value::Range(a), Value::Range(b)) => Value::Bool(a.scad_le(b)),
            (Value::Undef(_), Value::Undef(_)) => {
                Value::undef("operation undefined (undefined <= undefined)")
            }
            _ => Value::undef(format!(
                "undefined operation ({} <= {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// `>` — upstream `greater_visitor`.
    pub fn gt(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Bool(a > b),
            (Value::String(a), Value::String(b)) => Value::Bool(a > b),
            (Value::Bool(a), Value::Bool(b)) => Value::Bool(a > b),
            // Upstream VectorType::operator> is other < self.
            (Value::Vector(a), Value::Vector(b)) => vec_lt(b, a),
            (Value::Range(a), Value::Range(b)) => Value::Bool(a.scad_gt(b)),
            (Value::Undef(_), Value::Undef(_)) => {
                Value::undef("operation undefined (undefined > undefined)")
            }
            _ => Value::undef(format!(
                "undefined operation ({} > {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// `>=` — upstream `greaterequal_visitor`.
    pub fn ge(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Bool(a >= b),
            (Value::String(a), Value::String(b)) => Value::Bool(a >= b),
            (Value::Bool(a), Value::Bool(b)) => Value::Bool(a >= b),
            // Upstream VectorType::operator>= is !(self < other).
            (Value::Vector(a), Value::Vector(b)) => negate_comparison(vec_lt(a, b)),
            (Value::Range(a), Value::Range(b)) => Value::Bool(a.scad_ge(b)),
            (Value::Undef(_), Value::Undef(_)) => {
                Value::undef("operation undefined (undefined >= undefined)")
            }
            _ => Value::undef(format!(
                "undefined operation ({} >= {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// `+` — upstream `plus_visitor`: numbers, or element-wise vectors
    /// truncated to the shorter operand (upstream iterates while both
    /// iterators are valid, with a FIXME acknowledging the truncation).
    pub fn add(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Number(a + b),
            (Value::Vector(a), Value::Vector(b)) => Value::from(
                a.iter()
                    .zip(b.iter())
                    .map(|(x, y)| x.add(y))
                    .collect::<Vec<_>>(),
            ),
            _ => Value::undef(format!(
                "undefined operation ({} + {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// `-` — upstream `minus_visitor` (same shapes as `+`).
    pub fn sub(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Number(a - b),
            (Value::Vector(a), Value::Vector(b)) => Value::from(
                a.iter()
                    .zip(b.iter())
                    .map(|(x, y)| x.sub(y))
                    .collect::<Vec<_>>(),
            ),
            _ => Value::undef(format!(
                "undefined operation ({} - {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// `*` — upstream `multiply_visitor`: number product, scalar-vector
    /// scaling (recursive), and vector*vector dispatched on the operands'
    /// first element types: dot product, matrix*vector, vector*matrix, or
    /// matrix*matrix.
    pub fn mul(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Number(a * b),
            (Value::Number(_), Value::Vector(v)) => mult_vec_num(v, self),
            (Value::Vector(v), Value::Number(_)) => mult_vec_num(v, other),
            (Value::Vector(a), Value::Vector(b)) => mult_vectors(a, b),
            _ => Value::undef(format!(
                "undefined operation ({} * {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// `/` — upstream `Value::operator/`: numbers, vector/number and
    /// number/vector element-wise (both directions are defined upstream).
    pub fn div(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Number(a / b),
            (Value::Vector(v), Value::Number(_)) => {
                Value::from(v.iter().map(|e| e.div(other)).collect::<Vec<_>>())
            }
            (Value::Number(_), Value::Vector(v)) => {
                Value::from(v.iter().map(|e| self.div(e)).collect::<Vec<_>>())
            }
            _ => Value::undef(format!(
                "undefined operation ({} / {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// `%` — upstream `Value::operator%`: `fmod` on numbers only.
    pub fn modulo(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Number(a % b),
            _ => Value::undef(format!(
                "undefined operation ({} % {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// `^` — upstream `Value::operator^`: `pow` on numbers only.
    pub fn pow(&self, other: &Value) -> Value {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Number(a.powf(*b)),
            _ => Value::undef(format!(
                "undefined operation ({} ^ {})",
                self.type_name(),
                other.type_name()
            )),
        }
    }

    /// Unary `-` — upstream `Value::operator-`: numbers and vectors
    /// (recursive element-wise).
    pub fn neg(&self) -> Value {
        match self {
            Value::Number(n) => Value::Number(-n),
            Value::Vector(v) => Value::from(v.iter().map(Value::neg).collect::<Vec<_>>()),
            _ => Value::undef(format!("undefined operation (-{})", self.type_name())),
        }
    }

    /// Logical `!` — always a bool, via [`Value::to_bool`].
    pub fn not(&self) -> Value {
        Value::Bool(!self.to_bool())
    }

    /// Bracket indexing — upstream `bracket_visitor`.
    ///
    /// Strings index by UTF-8 code point (upstream `str_utf8_wrapper`
    /// offsets by character, not byte); out-of-bounds string access yields
    /// a plain undef with no reason, matching `Value::undefined.clone()`.
    pub fn index(&self, idx: &Value) -> Value {
        match (self, idx) {
            (Value::String(s), Value::Number(n)) => {
                let i = convert_to_uint32(*n);
                match s.chars().nth(i as usize) {
                    Some(c) => Value::String(c.to_string().into()),
                    None => Value::Undef(None),
                }
            }
            (Value::Vector(v), Value::Number(n)) => {
                let i = convert_to_uint32(*n);
                match v.get(i as usize) {
                    Some(e) => e.clone(),
                    None => Value::undef(format!(
                        "index {i} out of bounds for vector of size {}",
                        v.len()
                    )),
                }
            }
            (Value::Range(r), Value::Number(n)) => match convert_to_uint32(*n) {
                0 => Value::Number(r.start),
                1 => Value::Number(r.step),
                2 => Value::Number(r.end),
                _ => Value::undef(
                    "subscript operator only defined for indices 0-2 on range (begin,step,end)",
                ),
            },
            _ => Value::undef(format!(
                "undefined operation {}[{}]",
                self.type_name(),
                idx.type_name()
            )),
        }
    }

    /// Append an upstream-style reason to an undef (`UndefType::append`);
    /// reasons join with `\n\t` as in `UndefType::toString`.
    fn undef_append(self, why: &str) -> Value {
        match self {
            Value::Undef(Some(ref m)) => Value::undef(format!("{m}\n\t{why}")),
            Value::Undef(None) => Value::undef(why),
            other => other,
        }
    }
}

/// OpenSCAD `==` (upstream `equals_visitor`): cross-type comparisons are
/// `false`, `undef == undef` is `true` (reasons ignored), numbers follow
/// IEEE (`NaN != NaN`), vectors compare element-wise, ranges by
/// begin/step/`num_values()`.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Undef(_), Value::Undef(_)) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Vector(a), Value::Vector(b)) => a == b,
            (Value::Range(a), Value::Range(b)) => a == b,
            // Upstream compares function values by identity: copies of one
            // value are equal, two evaluations of a literal are not.
            (Value::Function(a), Value::Function(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Upstream `Value::toString()`: strings render unquoted at top level but
/// quoted inside vectors (`tostream_visitor`).
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Undef(_) => f.write_str("undef"),
            Value::Bool(b) => f.write_str(if *b { "true" } else { "false" }),
            Value::Number(n) => f.write_str(&format_number(*n)),
            Value::String(s) => f.write_str(s),
            Value::Vector(_) => fmt_element(self, f),
            Value::Range(r) => write!(f, "{r}"),
            Value::Function(function) => write!(f, "{function}"),
        }
    }
}

/// Vector-element formatting (upstream `tostream_visitor`): like `Display`
/// but strings are quoted.
fn fmt_element(v: &Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match v {
        Value::String(s) => write!(f, "\"{s}\""),
        Value::Vector(items) => {
            f.write_str("[")?;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                fmt_element(item, f)?;
            }
            f.write_str("]")
        }
        other => write!(f, "{other}"),
    }
}

/// Lexicographic vector `<` (upstream `VectorType::operator<`): element
/// comparisons that are undef propagate with an appended reason; a shorter
/// prefix sorts first.
fn vec_lt(a: &[Value], b: &[Value]) -> Value {
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let t = x.lt(y);
        if t.is_undef() {
            return t.undef_append(&format!("in vector comparison at index {i}"));
        }
        if t.to_bool() {
            return Value::Bool(true);
        }
        if y.lt(x).to_bool() {
            return Value::Bool(false);
        }
    }
    // Equal over the common prefix: (first1==last1) && (first2!=last2).
    Value::Bool(a.len() < b.len())
}

/// `!cmp` preserving undef (used for vector `<=` / `>=`).
fn negate_comparison(cmp: Value) -> Value {
    if cmp.is_undef() {
        cmp
    } else {
        Value::Bool(!cmp.to_bool())
    }
}

/// Scalar * vector, recursive (upstream `multvecnum`).
fn mult_vec_num(vec: &[Value], num: &Value) -> Value {
    Value::from(vec.iter().map(|v| v.mul(num)).collect::<Vec<_>>())
}

/// Vector * vector dispatch (upstream `multiply_visitor` VectorType case):
/// keyed on the type of each operand's first element.
fn mult_vectors(a: &[Value], b: &[Value]) -> Value {
    if a.is_empty() || b.is_empty() {
        return Value::undef("Multiplication is undefined on empty vectors");
    }
    match (&a[0], &b[0]) {
        (Value::Number(_), Value::Number(_)) => {
            if a.len() == b.len() {
                dot_product(a, b)
            } else {
                Value::undef(format!(
                    "vector*vector requires matching lengths ({} != {})",
                    a.len(),
                    b.len()
                ))
            }
        }
        (Value::Number(_), Value::Vector(_)) => {
            if a.len() == b.len() {
                mult_vec_mat(a, b)
            } else {
                Value::undef(format!(
                    "vector*matrix requires vector length to match matrix row count ({} != {})",
                    a.len(),
                    b.len()
                ))
            }
        }
        (Value::Vector(row0), Value::Number(_)) => {
            if row0.len() == b.len() {
                mult_mat_vec(a, b)
            } else {
                Value::undef(format!(
                    "matrix*vector requires matrix column count to match vector length ({} != {})",
                    row0.len(),
                    b.len()
                ))
            }
        }
        (Value::Vector(row0), Value::Vector(_)) => {
            if row0.len() == b.len() {
                mult_mat_mat(a, b)
            } else {
                Value::undef(format!(
                    "matrix*matrix requires left operand column count to match right operand row count ({} != {})",
                    row0.len(),
                    b.len()
                ))
            }
        }
        _ => Value::undef(format!(
            "undefined vector*vector multiplication where first elements are types {} and {}",
            a[0].type_name(),
            b[0].type_name()
        )),
    }
}

/// Dot product (upstream `multvecvec`); caller guarantees equal lengths.
fn dot_product(a: &[Value], b: &[Value]) -> Value {
    let mut r = 0.0;
    for (x, y) in a.iter().zip(b) {
        match (x, y) {
            (Value::Number(x), Value::Number(y)) => r += x * y,
            _ => {
                return Value::undef(format!(
                    "undefined operation ({} * {})",
                    x.type_name(),
                    y.type_name()
                ))
            }
        }
    }
    Value::Number(r)
}

/// Matrix * vector (upstream `multmatvec`).
fn mult_mat_vec(mat: &[Value], vec: &[Value]) -> Value {
    let mut out = Vec::with_capacity(mat.len());
    for (i, row_val) in mat.iter().enumerate() {
        let row = match row_val {
            Value::Vector(r) if r.len() == vec.len() => r,
            _ => return Value::undef(format!("Matrix must be rectangular. Problem at row {i}")),
        };
        let mut r = 0.0;
        for (j, (m, v)) in row.iter().zip(vec).enumerate() {
            let m = match m {
                Value::Number(m) => *m,
                _ => {
                    return Value::undef(format!(
                        "Matrix must contain only numbers. Problem at row {i}, col {j}"
                    ))
                }
            };
            let v = match v {
                Value::Number(v) => *v,
                _ => {
                    return Value::undef(format!(
                        "Vector must contain only numbers. Problem at index {j}"
                    ))
                }
            };
            r += m * v;
        }
        out.push(Value::Number(r));
    }
    Value::from(out)
}

/// Vector * matrix (upstream `multvecmat`); caller guarantees
/// `vec.len() == mat.len()` and that `mat[0]` is a vector.
fn mult_vec_mat(vec: &[Value], mat: &[Value]) -> Value {
    let first_row_size = match &mat[0] {
        Value::Vector(r) => r.len(),
        _ => 0, // upstream toVector() degrades non-vectors to EMPTY
    };
    let mut out = Vec::with_capacity(first_row_size);
    for i in 0..first_row_size {
        let mut r = 0.0;
        for j in 0..vec.len() {
            let row = match &mat[j] {
                Value::Vector(row) if row.len() == first_row_size => row,
                _ => {
                    return Value::undef(format!("Matrix must be rectangular. Problem at row {j}"))
                }
            };
            let v = match &vec[j] {
                Value::Number(v) => *v,
                _ => {
                    return Value::undef(format!(
                        "Vector must contain only numbers. Problem at index {j}"
                    ))
                }
            };
            let m = match &row[i] {
                Value::Number(m) => *m,
                _ => {
                    return Value::undef(format!(
                        "Matrix must contain only numbers. Problem at row {j}, col {i}"
                    ))
                }
            };
            r += v * m;
        }
        out.push(Value::Number(r));
    }
    Value::from(out)
}

/// Matrix * matrix (upstream `multiply_visitor` Matrix*Matrix branch).
fn mult_mat_mat(a: &[Value], b: &[Value]) -> Value {
    let mut out = Vec::with_capacity(a.len());
    for (i, src_row) in a.iter().enumerate() {
        let row: &[Value] = match src_row {
            Value::Vector(r) => r,
            _ => &[], // upstream toVector() degrades non-vectors to EMPTY
        };
        if row.len() != b.len() {
            return Value::undef(format!(
                "matrix*matrix left operand row length does not match right operand row count ({} != {}) at row {i}",
                row.len(),
                b.len()
            ));
        }
        let t = mult_vec_mat(row, b);
        if t.is_undef() {
            return t.undef_append(&format!("while processing left operand at row {i}"));
        }
        out.push(t);
    }
    Value::from(out)
}

/// Upstream `convert_to_uint32`: finite, in-range doubles truncate
/// (boost::numeric_cast's default Float2IntRounder is Trunc); everything
/// else maps to `u32::MAX`.
fn convert_to_uint32(d: f64) -> u32 {
    if d.is_finite() && d > -1.0 && d <= f64::from(u32::MAX) {
        d as u32
    } else {
        u32::MAX
    }
}

/// Number-to-string conversion matching upstream `DoubleConvert`
/// (double-conversion `ToPrecision` with 6 significant digits,
/// decimal notation while the decimal point stays within [-4, 6]
/// per DC_MAX_LEADING_ZEROES=5 / DC_MAX_TRAILING_ZEROES=0, explicit
/// '+' on positive exponents, unique zero, then trailing-zero trimming).
pub fn format_number(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    if v == 0.0 {
        return "0".to_string(); // UNIQUE_ZERO: -0.0 prints as 0
    }
    const PRECISION: usize = 6; // DC_PRECISION_REQUESTED
                                // 6 significant digits, correctly rounded.
    let sci = format!("{:.*e}", PRECISION - 1, v);
    let (mantissa, exp) = sci.split_once('e').expect("scientific notation");
    let exp: i32 = exp.parse().expect("exponent");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    debug_assert_eq!(digits.len(), PRECISION);
    let decimal_point = exp + 1;

    let mut out = String::new();
    if mantissa.starts_with('-') {
        out.push('-');
    }
    if (-4..=PRECISION as i32).contains(&decimal_point) {
        // Decimal notation.
        if decimal_point <= 0 {
            out.push_str("0.");
            for _ in 0..-decimal_point {
                out.push('0');
            }
            out.push_str(&digits);
            trim_trailing_zeroes(&mut out);
        } else {
            let dp = decimal_point as usize;
            out.push_str(&digits[..dp]);
            if dp < PRECISION {
                out.push('.');
                out.push_str(&digits[dp..]);
                trim_trailing_zeroes(&mut out);
            }
        }
    } else {
        // Exponential notation; at least one digit survives since v != 0.
        let trimmed = digits.trim_end_matches('0');
        out.push_str(&trimmed[..1]);
        if trimmed.len() > 1 {
            out.push('.');
            out.push_str(&trimmed[1..]);
        }
        let e = decimal_point - 1;
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&e.abs().to_string());
    }
    out
}

/// Upstream `trimTrailingZeroes`: strip zeroes after the decimal point,
/// then a dangling point.
fn trim_trailing_zeroes(s: &mut String) {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
}
