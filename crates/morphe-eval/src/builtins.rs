//! The OpenSCAD builtin function library.
//!
//! Semantics are ported from upstream openscad
//! `src/core/builtin_functions.cc` and `src/utils/degree_trig.cc` (master,
//! fetched 2026-07-03) and cross-checked against the installed OpenSCAD
//! 2021.01 binary via `echo(...)`. Builtins here are pure functions of
//! already-evaluated argument [`Value`]s: [`call_builtin`] dispatches by
//! name and returns the result plus any warnings, which the caller
//! ([`crate::eval::Evaluator::eval_call`]) routes to its warning sink.
//! `is_undef` is the one exception — it needs to suppress the
//! "unknown variable" warning for a bare-identifier argument, so it is
//! handled in the evaluator with access to the scope
//! (`builtin_functions.cc builtin_is_undef` special-cases a `Lookup`
//! argument via `try_lookup_variable`).
//!
//! Argument-count and argument-type misuse yields undef plus a warning
//! whose text matches the installed 2021.01 binary
//! (`"<name>() number of parameters does not match"` for arity and
//! `"<name>() parameter could not be converted"` for a type mismatch;
//! upstream master routes these through `print_argCnt_warning` /
//! `print_argConvert_positioned_warning`).
//!
//! Documented divergences from upstream:
//!
//! - `min`/`max` of a single vector containing a non-number: master's
//!   `min_max_arguments` breaks on the first non-number and returns undef;
//!   the 2021.01 binary silently drops it (`min([1,"x"])` -> `1`). We follow
//!   master (undef).
//! - `rands` uses a deterministic SplitMix64 stand-in, NOT boost/std
//!   `mt19937`. The seeded 4-argument form is reproducible but its numbers
//!   differ from upstream; the unseeded 3-argument form (which upstream
//!   seeds non-deterministically from wall-clock time) here uses a fixed
//!   seed so it too is deterministic.
//! - `version()`/`version_num()` return a fixed `[2021, 1, 0]` / `20210100`,
//!   matching the installed 2021.01 binary (`version.h` constants).

use crate::value::{format_number, Value};

/// Fixed version stand-in, matching the installed OpenSCAD 2021.01 binary's
/// `version()` == `[2021, 1, 0]` (upstream reads `openscad_version_*` from
/// the generated `version.h`).
const VERSION: [f64; 3] = [2021.0, 1.0, 0.0];

// Degree-trig constants (upstream `degree_trig.cc` / `<cmath>` `M_*`).
const DEG2RAD: f64 = std::f64::consts::PI / 180.0;
const RAD2DEG: f64 = 180.0 / std::f64::consts::PI;
const SQRT1_2: f64 = std::f64::consts::FRAC_1_SQRT_2; // sin/cos 45
                                                      // `M_SQRT3_4` = sqrt(3)/2, `M_SQRT1_3` = 1/sqrt(3), `M_SQRT3` = sqrt(3).
const SQRT3_4: f64 = 0.866_025_403_784_438_6;
const SQRT1_3: f64 = 0.577_350_269_189_625_8;
const SQRT3: f64 = 1.732_050_807_568_877_2;
// Upstream `TRIG_HUGE_VAL = (1<<26) * 360.0 * (1<<26)`: past it, degree
// reduction loses all accuracy, so the result is NaN.
const TRIG_HUGE_VAL: f64 = ((1_i64 << 26) as f64) * 360.0 * ((1_i64 << 26) as f64);

/// Whether `name` names a builtin function (so the evaluator dispatches to
/// it rather than warning "unknown function"). Kept in sync with the
/// [`call_builtin`] match and the `is_undef` special case.
pub(crate) fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "abs"
            | "sign"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "atan2"
            | "floor"
            | "ceil"
            | "round"
            | "ln"
            | "log"
            | "pow"
            | "sqrt"
            | "exp"
            | "min"
            | "max"
            | "norm"
            | "cross"
            | "len"
            | "concat"
            | "lookup"
            | "search"
            | "str"
            | "chr"
            | "ord"
            | "is_undef"
            | "is_bool"
            | "is_num"
            | "is_string"
            | "is_list"
            | "is_function"
            | "rands"
            | "version"
            | "version_num"
    )
}

/// Dispatch a builtin call over already-evaluated `args`, appending any
/// warnings to `warnings`. Returns `None` only when `name` is not a builtin
/// (so the evaluator falls through to its unknown-function path). `is_undef`
/// is not handled here — see the module docs.
pub(crate) fn call_builtin(
    name: &str,
    args: &[Value],
    warnings: &mut Vec<String>,
) -> Option<Value> {
    let value = match name {
        // ---- scalar math (one number) -------------------------------
        "abs" => unary(name, args, warnings, f64::abs),
        "sign" => unary(name, args, warnings, |x| {
            if x < 0.0 {
                -1.0
            } else if x > 0.0 {
                1.0
            } else {
                0.0
            }
        }),
        "sin" => unary(name, args, warnings, sin_degrees),
        "cos" => unary(name, args, warnings, cos_degrees),
        "tan" => unary(name, args, warnings, tan_degrees),
        "asin" => unary(name, args, warnings, asin_degrees),
        "acos" => unary(name, args, warnings, acos_degrees),
        "atan" => unary(name, args, warnings, atan_degrees),
        "floor" => unary(name, args, warnings, f64::floor),
        "ceil" => unary(name, args, warnings, f64::ceil),
        "round" => unary(name, args, warnings, f64::round), // round-half-away
        "ln" => unary(name, args, warnings, f64::ln),
        "sqrt" => unary(name, args, warnings, f64::sqrt),
        "exp" => unary(name, args, warnings, f64::exp),
        "atan2" => binary(name, args, warnings, atan2_degrees),
        "pow" => binary(name, args, warnings, f64::powf),
        "log" => builtin_log(args, warnings),
        // ---- reductions / vectors -----------------------------------
        "min" => builtin_min_max(name, args, warnings, false),
        "max" => builtin_min_max(name, args, warnings, true),
        "norm" => builtin_norm(args, warnings),
        "cross" => builtin_cross(args, warnings),
        "len" => builtin_len(args, warnings),
        "concat" => builtin_concat(args),
        "lookup" => builtin_lookup(args, warnings),
        "search" => builtin_search(args, warnings),
        // ---- strings ------------------------------------------------
        "str" => builtin_str(args),
        "chr" => builtin_chr(args, warnings),
        "ord" => builtin_ord(args, warnings),
        // ---- type predicates ----------------------------------------
        "is_bool" => predicate(name, args, warnings, |v| matches!(v, Value::Bool(_))),
        "is_num" => predicate(
            name,
            args,
            warnings,
            |v| matches!(v, Value::Number(n) if !n.is_nan()),
        ),
        "is_string" => predicate(name, args, warnings, |v| matches!(v, Value::String(_))),
        "is_list" => predicate(name, args, warnings, |v| matches!(v, Value::Vector(_))),
        "is_function" => predicate(name, args, warnings, |v| matches!(v, Value::Function(_))),
        // ---- other --------------------------------------------------
        "rands" => builtin_rands(args, warnings),
        "version" => Value::from(
            VERSION
                .iter()
                .map(|&d| Value::Number(d))
                .collect::<Vec<_>>(),
        ),
        "version_num" => builtin_version_num(args),
        _ => return None,
    };
    Some(value)
}

// ------------------------------------------------------------ arg helpers

fn warn_arity(name: &str, warnings: &mut Vec<String>) {
    warnings.push(format!("{name}() number of parameters does not match"));
}

fn warn_convert(name: &str, warnings: &mut Vec<String>) {
    warnings.push(format!("{name}() parameter could not be converted"));
}

/// A unary numeric builtin: exactly one number argument.
fn unary(name: &str, args: &[Value], warnings: &mut Vec<String>, f: impl Fn(f64) -> f64) -> Value {
    match args {
        [Value::Number(x)] => Value::Number(f(*x)),
        [_] => {
            warn_convert(name, warnings);
            Value::Undef(None)
        }
        _ => {
            warn_arity(name, warnings);
            Value::Undef(None)
        }
    }
}

/// A binary numeric builtin: exactly two number arguments.
fn binary(
    name: &str,
    args: &[Value],
    warnings: &mut Vec<String>,
    f: impl Fn(f64, f64) -> f64,
) -> Value {
    match args {
        [Value::Number(a), Value::Number(b)] => Value::Number(f(*a, *b)),
        [_, _] => {
            warn_convert(name, warnings);
            Value::Undef(None)
        }
        _ => {
            warn_arity(name, warnings);
            Value::Undef(None)
        }
    }
}

/// A type predicate: exactly one argument of any type.
fn predicate(
    name: &str,
    args: &[Value],
    warnings: &mut Vec<String>,
    f: impl Fn(&Value) -> bool,
) -> Value {
    match args {
        [v] => Value::Bool(f(v)),
        _ => {
            warn_arity(name, warnings);
            Value::Undef(None)
        }
    }
}

/// Borrow a value's elements as a slice, treating non-vectors as empty
/// (upstream `Value::toVector()` returns a static empty vector).
fn as_slice(v: &Value) -> &[Value] {
    match v {
        Value::Vector(items) => items,
        _ => &[],
    }
}

// ------------------------------------------------------------ log / reduce

/// `log(x)` is base 10; `log(base, x)` is `log(x) / log(base)`
/// (upstream `builtin_log`).
fn builtin_log(args: &[Value], warnings: &mut Vec<String>) -> Value {
    let (base, x) = match args {
        [Value::Number(x)] => (10.0, *x),
        [Value::Number(base), Value::Number(x)] => (*base, *x),
        [_] | [_, _] => {
            warn_convert("log", warnings);
            return Value::Undef(None);
        }
        _ => {
            warn_arity("log", warnings);
            return Value::Undef(None);
        }
    };
    Value::Number(x.ln() / base.ln())
}

/// `min`/`max`: variadic numbers, or a single vector of numbers (upstream
/// `min_max_arguments` + `builtin_min`/`builtin_max`). Empty, or any
/// non-number, yields undef with a warning.
///
/// The reduction replicates `std::min_element` / `std::max_element`, not
/// `f64::min`/`f64::max`: those drop NaN, whereas the standard-library
/// element search keeps the running result unless the next element compares
/// strictly greater (max) or strictly less (min). Because every comparison
/// with NaN is false, a NaN *accumulator* is never displaced — so
/// `max([0/0, 5])` is `NaN` while `max([5, 0/0])` is `5` (verified against
/// the OpenSCAD 2021.01 binary).
fn builtin_min_max(name: &str, args: &[Value], warnings: &mut Vec<String>, is_max: bool) -> Value {
    let elements: &[Value] = match args {
        [] => {
            warn_arity(name, warnings);
            return Value::Undef(None);
        }
        // Single vector argument: reduce over its elements.
        [Value::Vector(v)] => {
            if v.is_empty() {
                warn_arity(name, warnings);
                return Value::Undef(None);
            }
            v
        }
        other => other,
    };
    let mut acc: Option<f64> = None;
    for element in elements {
        let Value::Number(x) = element else {
            // Master: break on any non-number -> undef.
            warn_convert(name, warnings);
            return Value::Undef(None);
        };
        acc = Some(match acc {
            None => *x,
            // std::max_element keeps `a` unless `a < x`; std::min_element
            // keeps `a` unless `x < a`.
            Some(a) if is_max => {
                if a < *x {
                    *x
                } else {
                    a
                }
            }
            Some(a) => {
                if *x < a {
                    *x
                } else {
                    a
                }
            }
        });
    }
    Value::Number(acc.expect("non-empty by construction"))
}

/// `norm(vector)`: Euclidean length of a numeric vector (upstream
/// `builtin_norm`).
fn builtin_norm(args: &[Value], warnings: &mut Vec<String>) -> Value {
    let [Value::Vector(v)] = args else {
        // 2021.01 binary: a wrong argument *count* warns "number of
        // parameters does not match", but a single non-vector argument
        // (e.g. `norm(5)`) is undef *silently* — no type-mismatch warning.
        if args.len() != 1 {
            warn_arity("norm", warnings);
        }
        return Value::Undef(None);
    };
    let mut sum = 0.0;
    for element in v.iter() {
        let Value::Number(x) = element else {
            warnings.push("Incorrect arguments to norm()".to_string());
            return Value::Undef(None);
        };
        sum += x * x;
    }
    Value::Number(sum.sqrt())
}

/// `cross(a, b)`: 3D cross product (a 3-vector) or the scalar z-component of
/// a 2D cross (upstream `builtin_cross`). NaN/Inf components yield undef.
fn builtin_cross(args: &[Value], warnings: &mut Vec<String>) -> Value {
    let [Value::Vector(v0), Value::Vector(v1)] = args else {
        // 2021.01 binary: two non-vector arguments give a type message, a
        // wrong count gives a count message (both distinct from the generic
        // convert/arity texts).
        if args.len() == 2 {
            warnings.push("Invalid type of parameters for cross()".to_string());
        } else {
            warnings.push("Invalid number of parameters for cross()".to_string());
        }
        return Value::Undef(None);
    };
    if v0.len() == 2 && v1.len() == 2 {
        let (Value::Number(a0), Value::Number(a1), Value::Number(b0), Value::Number(b1)) =
            (&v0[0], &v0[1], &v1[0], &v1[1])
        else {
            warnings.push("Invalid value in parameter vector for cross()".to_string());
            return Value::Undef(None);
        };
        return Value::Number(a0 * b1 - a1 * b0);
    }
    if v0.len() != 3 || v1.len() != 3 {
        warnings.push("Invalid vector size of parameter for cross()".to_string());
        return Value::Undef(None);
    }
    let mut a = [0.0; 3];
    let mut b = [0.0; 3];
    for i in 0..3 {
        let (Value::Number(x), Value::Number(y)) = (&v0[i], &v1[i]) else {
            warnings.push("Invalid value in parameter vector for cross()".to_string());
            return Value::Undef(None);
        };
        if x.is_nan() || y.is_nan() {
            warnings.push("Invalid value (NaN) in parameter vector for cross()".to_string());
            return Value::Undef(None);
        }
        if x.is_infinite() || y.is_infinite() {
            warnings.push("Invalid value (INF) in parameter vector for cross()".to_string());
            return Value::Undef(None);
        }
        a[i] = *x;
        b[i] = *y;
    }
    Value::from(vec![
        Value::Number(a[1] * b[2] - a[2] * b[1]),
        Value::Number(a[2] * b[0] - a[0] * b[2]),
        Value::Number(a[0] * b[1] - a[1] * b[0]),
    ])
}

/// `len(string)` counts UTF-8 glyphs, `len(vector)` its length; anything
/// else is undef with a warning (upstream `builtin_length`).
fn builtin_len(args: &[Value], warnings: &mut Vec<String>) -> Value {
    match args {
        [Value::Vector(v)] => Value::Number(v.len() as f64),
        [Value::String(s)] => Value::Number(s.chars().count() as f64),
        [_] => {
            warn_convert("len", warnings);
            Value::Undef(None)
        }
        _ => {
            warn_arity("len", warnings);
            Value::Undef(None)
        }
    }
}

/// `concat(...)`: flatten one level — vector arguments splice their elements,
/// everything else appends as-is (upstream `builtin_concat`).
fn builtin_concat(args: &[Value]) -> Value {
    let mut out = Vec::new();
    for arg in args {
        match arg {
            Value::Vector(v) => out.extend(v.iter().cloned()),
            other => out.push(other.clone()),
        }
    }
    Value::from(out)
}

/// `str(...)`: concatenate every argument's `toString` form (upstream
/// `builtin_str`); strings render unquoted, numbers via `format_number`.
fn builtin_str(args: &[Value]) -> Value {
    let mut out = String::new();
    for arg in args {
        out.push_str(&arg.to_string());
    }
    Value::String(out.into())
}

/// `chr(...)`: map each numeric code point (from numbers, vectors, or
/// ranges) to its UTF-8 character and concatenate (upstream `builtin_chr` /
/// `Value::chrString`). Non-positive, zero, or invalid code points
/// contribute nothing.
fn builtin_chr(args: &[Value], warnings: &mut Vec<String>) -> Value {
    let mut out = String::new();
    for arg in args {
        chr_append(arg, &mut out, warnings);
    }
    Value::String(out.into())
}

fn chr_append(value: &Value, out: &mut String, warnings: &mut Vec<String>) {
    match value {
        Value::Number(n) => push_code_point(*n, out),
        Value::Vector(v) => {
            for element in v.iter() {
                chr_append(element, out, warnings);
            }
        }
        Value::Range(r) => {
            // Upstream guards a chr range at RangeType::MAX_RANGE_STEPS.
            if r.num_values() >= 10_000 {
                warnings.push(format!(
                    "Bad range parameter in for statement: too many elements ({})",
                    r.num_values()
                ));
                return;
            }
            for d in r.iter() {
                push_code_point(d, out);
            }
        }
        _ => {}
    }
}

/// Append one `chr` code point (upstream `chr_visitor::operator()(double)`):
/// require `v > 0`, truncate to `gunichar` — a `uint32_t`, so the C cast
/// *wraps* modulo 2^32 rather than saturating (`chr(0x100000041)` -> `"A"`,
/// verified against the 2021.01 binary) — then skip 0 and invalid points.
fn push_code_point(v: f64, out: &mut String) {
    if v > 0.0 {
        let c = v as i64 as u32; // truncate toward zero, then wrap mod 2^32
        if c != 0 {
            if let Some(ch) = char::from_u32(c) {
                out.push(ch);
            }
        }
    }
}

/// `ord(string)`: the code point of the first character; empty string or a
/// non-string yields undef (upstream `builtin_ord`).
fn builtin_ord(args: &[Value], warnings: &mut Vec<String>) -> Value {
    let [Value::String(s)] = args else {
        // 2021.01 binary: a single non-string argument names it in the
        // message; a wrong count reports the count instead.
        match args {
            [only] => warnings.push(format!("ord() argument {only} is not of type string")),
            _ => warnings.push(format!(
                "ord() called with {} arguments, only 1 argument expected",
                args.len()
            )),
        }
        return Value::Undef(None);
    };
    match s.chars().next() {
        Some(c) => Value::Number(c as u32 as f64),
        None => Value::Undef(None),
    }
}

// ------------------------------------------------------------------ lookup

/// Interpret a value as a `[key, value]` numeric pair (upstream
/// `Value::getVec2`: a vector of exactly two numbers).
fn get_vec2(v: &Value) -> Option<(f64, f64)> {
    match v {
        Value::Vector(items) if items.len() == 2 => match (&items[0], &items[1]) {
            (Value::Number(a), Value::Number(b)) => Some((*a, *b)),
            _ => None,
        },
        _ => None,
    }
}

/// `lookup(key, table)`: piecewise-linear interpolation over a table of
/// `[key, value]` pairs, clamping to the end values outside the key range
/// (upstream `builtin_lookup`).
fn builtin_lookup(args: &[Value], warnings: &mut Vec<String>) -> Value {
    let [Value::Number(p), Value::Vector(vec)] = args else {
        if args.len() == 2 {
            warn_convert("lookup", warnings);
        } else {
            warn_arity("lookup", warnings);
        }
        return Value::Undef(None);
    };
    let p = *p;
    if !p.is_finite() {
        warnings.push(format!(
            "lookup({}, ...) first argument is not a number",
            format_number(p)
        ));
        return Value::Undef(None);
    }

    let mut it = vec.iter();
    let Some((mut low_p, mut low_v)) = it.next().and_then(get_vec2) else {
        return Value::Undef(None);
    };
    let (mut high_p, mut high_v) = (low_p, low_v);

    for entry in it {
        if let Some((this_p, this_v)) = get_vec2(entry) {
            if this_p <= p && (this_p > low_p || low_p > p) {
                low_p = this_p;
                low_v = this_v;
            }
            if this_p >= p && (this_p < high_p || high_p < p) {
                high_p = this_p;
                high_v = this_v;
            }
        }
    }
    if p <= low_p {
        return Value::Number(high_v);
    }
    if p >= high_p {
        return Value::Number(low_v);
    }
    let f = (p - low_p) / (high_p - low_p);
    Value::Number(high_v * f + low_v * (1.0 - f))
}

// ------------------------------------------------------------------ search

/// Read an optional trailing numeric argument as an unsigned count
/// (upstream casts `arguments[i]->toDouble()` to `unsigned int`).
fn opt_u32(args: &[Value], index: usize, default: u32) -> u32 {
    match args.get(index) {
        Some(Value::Number(n)) if n.is_finite() && *n >= 0.0 => *n as u32,
        Some(_) => 0,
        None => default,
    }
}

/// The code point of a value's first character (upstream
/// `toStrUtf8Wrapper().get_utf8_char()`; non-strings coerce via toString).
fn first_char(value: &Value) -> Option<char> {
    match value {
        Value::String(s) => s.chars().next(),
        other => other.to_string().chars().next(),
    }
}

/// `search(find, table [, num_returns_per_match [, index_col_num]])`
/// (upstream `builtin_search`). Implements every documented mode:
/// number-in-vector, string-in-string, string-in-vector, and
/// vector(list-of-values)-in-vector.
fn builtin_search(args: &[Value], warnings: &mut Vec<String>) -> Value {
    if args.len() < 2 || args.len() > 4 {
        warn_arity("search", warnings);
        return Value::Undef(None);
    }
    let find = &args[0];
    let table = &args[1];
    let nrpm = opt_u32(args, 2, 1);
    let index_col = opt_u32(args, 3, 0) as usize;

    match find {
        Value::Number(_) => {
            let mut out = Vec::new();
            let mut match_count = 0u32;
            for (j, element) in as_slice(table).iter().enumerate() {
                if search_matches(find, element, index_col) {
                    out.push(Value::Number(j as f64));
                    match_count += 1;
                    if nrpm != 0 && match_count >= nrpm {
                        break;
                    }
                }
            }
            Value::from(out)
        }
        Value::String(find_str) => match table {
            Value::String(table_str) => search_str_str(find_str, table_str, nrpm),
            _ => search_str_vec(find_str, as_slice(table), nrpm, index_col, warnings),
        },
        Value::Vector(find_vec) => {
            let mut out = Vec::new();
            for find_value in find_vec.iter() {
                let mut match_count = 0u32;
                let mut result = Vec::new();
                for (j, element) in as_slice(table).iter().enumerate() {
                    if search_matches(find_value, element, index_col) {
                        match_count += 1;
                        if nrpm == 1 {
                            out.push(Value::Number(j as f64));
                            break;
                        }
                        result.push(Value::Number(j as f64));
                        if nrpm > 1 && match_count >= nrpm {
                            break;
                        }
                    }
                }
                if (nrpm == 1 && match_count == 0) || nrpm == 0 || nrpm > 1 {
                    out.push(Value::from(result));
                }
            }
            Value::from(out)
        }
        _ => Value::Undef(None),
    }
}

/// The match test shared by the number- and vector-find branches: equal to
/// the whole element (column 0), or to its `index_col`-th component.
fn search_matches(find: &Value, element: &Value, index_col: usize) -> bool {
    if index_col == 0 && find == element {
        return true;
    }
    let cols = as_slice(element);
    index_col < cols.len() && find == &cols[index_col]
}

/// String-in-string search: one result per character of `find` (upstream
/// `search(str, str, ...)`).
fn search_str_str(find: &str, table: &str, nrpm: u32) -> Value {
    let table_chars: Vec<char> = table.chars().collect();
    let mut out = Vec::new();
    for f in find.chars() {
        let mut match_count = 0u32;
        let mut result = Vec::new();
        for (j, &t) in table_chars.iter().enumerate() {
            if f == t {
                match_count += 1;
                if nrpm == 1 {
                    out.push(Value::Number(j as f64));
                    break;
                }
                result.push(Value::Number(j as f64));
                if nrpm > 1 && match_count >= nrpm {
                    break;
                }
            }
        }
        if nrpm == 0 || nrpm > 1 {
            out.push(Value::from(result));
        }
    }
    Value::from(out)
}

/// String-in-vector search: match each character of `find` against the
/// `index_col`-th column of each table entry (upstream
/// `search(str, vector, ...)`).
fn search_str_vec(
    find: &str,
    table: &[Value],
    nrpm: u32,
    index_col: usize,
    warnings: &mut Vec<String>,
) -> Value {
    let mut out = Vec::new();
    for f in find.chars() {
        let mut match_count = 0u32;
        let mut result = Vec::new();
        for (j, entry) in table.iter().enumerate() {
            let cols = as_slice(entry);
            if cols.len() <= index_col {
                // Master `search(str, vector, ...)`: a too-short entry warns
                // and aborts the whole search to an empty vector.
                warnings.push(format!(
                    "Invalid entry in search vector at index {j}, required number of values in the entry: {}. Invalid entry: {}",
                    index_col + 1,
                    entry.to_echo_string()
                ));
                return Value::from(Vec::new());
            }
            if first_char(&cols[index_col]) == Some(f) {
                match_count += 1;
                if nrpm == 1 {
                    out.push(Value::Number(j as f64));
                    break;
                }
                result.push(Value::Number(j as f64));
                if nrpm > 1 && match_count >= nrpm {
                    break;
                }
            }
        }
        if nrpm == 0 || nrpm > 1 {
            out.push(Value::from(result));
        }
    }
    Value::from(out)
}

// ---------------------------------------------------------------- version

/// `version_num()` == `y*10000 + m*100 + d`; with a vector argument it reads
/// that `[y, m, d]` instead (upstream `builtin_version_num` via `getVec3`).
///
/// `getVec3` accepts a 2- or 3-element vector, defaulting a missing day to 0
/// (`version_num([2019, 5])` -> `20190500`, verified against the 2021.01
/// binary); a scalar or any other length is undef.
fn builtin_version_num(args: &[Value]) -> Value {
    let (y, m, d) = match args {
        [] => (VERSION[0], VERSION[1], VERSION[2]),
        [Value::Vector(v)] if v.len() == 2 || v.len() == 3 => {
            let mut nums = [0.0; 3]; // day defaults to 0
            for (slot, element) in nums.iter_mut().zip(v.iter()) {
                match element {
                    Value::Number(n) => *slot = *n,
                    _ => return Value::Undef(None),
                }
            }
            (nums[0], nums[1], nums[2])
        }
        _ => return Value::Undef(None),
    };
    Value::Number(y * 10000.0 + m * 100.0 + d)
}

// ------------------------------------------------------------------ rands

/// Hard ceiling on the number of values one `rands()` call may produce — a
/// morphe hardening (upstream allocates whatever `count` asks, so
/// `rands(0,1,1e11)` aborts the process on allocation failure). Sized like
/// the evaluator's `1_000_000` loop/range caps (upstream's own
/// `for`-range and C-style comprehension limits): a count above it warns and
/// yields undef rather than a silently truncated vector.
pub const MAX_RANDS_COUNT: usize = 1_000_000;

/// `rands(min, max, count [, seed])`: a deterministic SplitMix64 stand-in
/// (NOT boost/std `mt19937` — see the module docs). Without a seed a fixed
/// seed is used so the result stays deterministic.
fn builtin_rands(args: &[Value], warnings: &mut Vec<String>) -> Value {
    if args.len() < 3 || args.len() > 4 {
        warn_arity("rands", warnings);
        return Value::Undef(None);
    }
    let nums: Option<Vec<f64>> = args
        .iter()
        .map(|v| match v {
            Value::Number(n) => Some(*n),
            _ => None,
        })
        .collect();
    let Some(nums) = nums else {
        warn_convert("rands", warnings);
        return Value::Undef(None);
    };

    let mut min = nums[0];
    if !min.is_finite() {
        warnings.push("rands() range min cannot be infinite".to_string());
        min = -f64::MAX / 2.0;
    }
    let mut max = nums[1];
    if !max.is_finite() {
        warnings.push("rands() range max cannot be infinite".to_string());
        max = f64::MAX / 2.0;
    }
    if max < min {
        std::mem::swap(&mut min, &mut max);
    }

    // Count: |count| (upstream `std::abs`), an infinite count warns and
    // yields one result (upstream), a NaN count warns and yields none, and a
    // count above MAX_RANDS_COUNT warns and yields undef. The cap is checked
    // on the f64 BEFORE the `as usize` cast (which saturates 1e300 to
    // usize::MAX and made `Vec::with_capacity` panic).
    let mut count_f = nums[2].abs();
    if count_f.is_nan() {
        warnings.push("rands() number of results is NaN; returning no results".to_string());
        count_f = 0.0;
    } else if count_f.is_infinite() {
        warnings.push("rands() cannot create an infinite number of results".to_string());
        count_f = 1.0;
    }
    if count_f > MAX_RANDS_COUNT as f64 {
        warnings.push(format!(
            "rands() number of results {count_f} exceeds the limit of {MAX_RANDS_COUNT}"
        ));
        return Value::Undef(None);
    }
    let count = count_f as usize;

    // Seed: the seed argument's raw bits, else a fixed default. Upstream
    // seeds from a boost hash / wall-clock time instead (documented
    // divergence).
    let mut state = match nums.get(3) {
        Some(seed) => seed.to_bits(),
        None => 0x853c_49e6_748f_ea9b,
    };

    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if min >= max {
            out.push(Value::Number(min));
        } else {
            let unit = split_mix64_unit(&mut state);
            out.push(Value::Number(min + unit * (max - min)));
        }
    }
    Value::from(out)
}

/// One SplitMix64 step returning a uniform `f64` in `[0, 1)`.
fn split_mix64_unit(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Top 53 bits -> [0, 1).
    (z >> 11) as f64 / (1_u64 << 53) as f64
}

// ---------------------------------------------------------- degree trig

/// `sin`, in degrees, exact at multiples of 30/45/60/90 (upstream
/// `sin_degrees`).
fn sin_degrees(mut x: f64) -> f64 {
    if (0.0..360.0).contains(&x) {
        // already reduced
    } else if x < TRIG_HUGE_VAL && x > -TRIG_HUGE_VAL {
        x -= 360.0 * (x / 360.0).floor();
    } else {
        return f64::NAN;
    }
    let oppose = x >= 180.0;
    if oppose {
        x -= 180.0;
    }
    if x > 90.0 {
        x = 180.0 - x;
    }
    let r = if x < 45.0 {
        if x == 30.0 {
            0.5
        } else {
            (x * DEG2RAD).sin()
        }
    } else if x == 45.0 {
        SQRT1_2
    } else if x == 60.0 {
        SQRT3_4
    } else {
        ((90.0 - x) * DEG2RAD).cos()
    };
    if oppose {
        -r
    } else {
        r
    }
}

/// `cos`, in degrees (upstream `cos_degrees`).
fn cos_degrees(mut x: f64) -> f64 {
    if (0.0..360.0).contains(&x) {
        // already reduced
    } else if x < TRIG_HUGE_VAL && x > -TRIG_HUGE_VAL {
        x -= 360.0 * (x / 360.0).floor();
    } else {
        return f64::NAN;
    }
    let mut oppose = x >= 180.0;
    if oppose {
        x -= 180.0;
    }
    if x > 90.0 {
        x = 180.0 - x;
        oppose = !oppose;
    }
    let r = if x > 45.0 {
        if x == 60.0 {
            0.5
        } else {
            ((90.0 - x) * DEG2RAD).sin()
        }
    } else if x == 45.0 {
        SQRT1_2
    } else if x == 30.0 {
        SQRT3_4
    } else {
        (x * DEG2RAD).cos()
    };
    if oppose {
        -r
    } else {
        r
    }
}

/// `tan`, in degrees, with signed infinities at 90 + 180k (upstream
/// `tan_degrees`).
fn tan_degrees(mut x: f64) -> f64 {
    let cycles = (x / 180.0).floor();
    if (0.0..180.0).contains(&x) {
        // already reduced
    } else if x < TRIG_HUGE_VAL && x > -TRIG_HUGE_VAL {
        x -= 180.0 * cycles;
    } else {
        return f64::NAN;
    }
    let oppose = x > 90.0;
    if oppose {
        x = 180.0 - x;
    }
    let even = (cycles as i64) % 2 == 0;
    let r = if x == 0.0 {
        if even {
            0.0
        } else {
            -0.0
        }
    } else if x == 30.0 {
        SQRT1_3
    } else if x == 45.0 {
        1.0
    } else if x == 60.0 {
        SQRT3
    } else if x == 90.0 {
        if even {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        }
    } else {
        (x * DEG2RAD).tan()
    };
    if oppose {
        -r
    } else {
        r
    }
}

/// `asin`, returning degrees, snapping to a whole degree when exact
/// (upstream `asin_degrees`).
fn asin_degrees(x: f64) -> f64 {
    let degs = RAD2DEG * x.asin();
    let whole = degs.round();
    if sin_degrees(whole) == x {
        whole
    } else {
        degs
    }
}

/// `acos`, returning degrees (upstream `acos_degrees`).
fn acos_degrees(x: f64) -> f64 {
    let degs = RAD2DEG * x.acos();
    let whole = degs.round();
    if cos_degrees(whole) == x {
        whole
    } else {
        degs
    }
}

/// `atan`, returning degrees (upstream `atan_degrees`).
fn atan_degrees(x: f64) -> f64 {
    let degs = RAD2DEG * x.atan();
    let whole = degs.round();
    if tan_degrees(whole) == x {
        whole
    } else {
        degs
    }
}

/// `atan2`, returning degrees, snapping to a whole degree within a tight
/// tolerance (upstream `atan2_degrees`).
fn atan2_degrees(y: f64, x: f64) -> f64 {
    let degs = RAD2DEG * y.atan2(x);
    let whole = degs.round();
    if (degs - whole).abs() < 3.0e-14 {
        whole
    } else {
        degs
    }
}
