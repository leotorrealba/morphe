//! OpenSCAD value-model conformance tests.
//!
//! Every rule here is transcribed from upstream openscad
//! src/core/Value.cc, Value.h, RangeType.h, UndefType.h and
//! str_utf8_wrapper.h (master, fetched 2026-07-02).

use morphe_eval::value::{format_number, Range, Value};

fn n(x: f64) -> Value {
    Value::Number(x)
}

fn s(x: &str) -> Value {
    Value::String(x.into())
}

fn nums(xs: &[f64]) -> Value {
    Value::from(xs.iter().map(|&x| Value::Number(x)).collect::<Vec<_>>())
}

fn range(start: f64, step: f64, end: f64) -> Value {
    Value::Range(Range::new(start, step, end))
}

fn undef() -> Value {
    Value::Undef(None)
}

// ---------------------------------------------------------------------------
// Truthiness — upstream Value::toBool() (Value.cc)
// ---------------------------------------------------------------------------

#[test]
fn undef_is_falsy() {
    assert!(!undef().to_bool());
    assert!(!Value::undef("some reason").to_bool());
}

#[test]
fn bool_truthiness_is_identity() {
    assert!(Value::Bool(true).to_bool());
    assert!(!Value::Bool(false).to_bool());
}

#[test]
fn number_truthiness_is_nonzero() {
    assert!(!n(0.0).to_bool());
    assert!(!n(-0.0).to_bool());
    assert!(n(1.0).to_bool());
    assert!(n(-1.0).to_bool());
    assert!(n(0.5).to_bool());
}

#[test]
fn nan_is_truthy() {
    // Upstream: `std::get<double>(this->value) != 0` — NaN != 0 is true.
    assert!(n(f64::NAN).to_bool());
}

#[test]
fn string_truthiness_is_nonempty() {
    assert!(!s("").to_bool());
    assert!(s("a").to_bool());
    assert!(s("0").to_bool()); // no numeric parsing
    assert!(s("false").to_bool());
}

#[test]
fn vector_truthiness_is_nonempty() {
    assert!(!Value::from(vec![]).to_bool());
    assert!(nums(&[0.0]).to_bool()); // [0] is truthy: non-empty
    assert!(Value::from(vec![Value::Bool(false)]).to_bool());
}

#[test]
fn range_is_always_truthy() {
    assert!(range(0.0, 1.0, 10.0).to_bool());
    assert!(range(10.0, 1.0, 0.0).to_bool()); // even when it iterates empty
    assert!(range(0.0, 0.0, 10.0).to_bool());
}

// ---------------------------------------------------------------------------
// Equality — upstream equals_visitor: cross-type => false, undef==undef => true
// ---------------------------------------------------------------------------

#[test]
fn undef_equals_undef() {
    assert_eq!(undef(), undef());
    // Messages are diagnostic payload only; equality ignores them.
    assert_eq!(Value::undef("reason a"), Value::undef("reason b"));
}

#[test]
fn cross_type_equality_is_false() {
    assert_ne!(n(0.0), Value::Bool(false));
    assert_ne!(n(1.0), s("1"));
    assert_ne!(undef(), Value::Bool(false));
    assert_ne!(Value::from(vec![]), s(""));
    assert_ne!(range(0.0, 1.0, 2.0), nums(&[0.0, 1.0, 2.0]));
}

#[test]
fn number_equality() {
    assert_eq!(n(1.0), n(1.0));
    assert_eq!(n(0.0), n(-0.0));
    assert_ne!(n(1.0), n(2.0));
    assert_ne!(n(f64::NAN), n(f64::NAN)); // IEEE semantics as upstream
}

#[test]
fn string_and_bool_equality() {
    assert_eq!(s("abc"), s("abc"));
    assert_ne!(s("abc"), s("abd"));
    assert_eq!(Value::Bool(true), Value::Bool(true));
    assert_ne!(Value::Bool(true), Value::Bool(false));
}

#[test]
fn vector_equality_is_elementwise() {
    assert_eq!(nums(&[1.0, 2.0]), nums(&[1.0, 2.0]));
    assert_ne!(nums(&[1.0, 2.0]), nums(&[1.0, 3.0]));
    assert_ne!(nums(&[1.0, 2.0]), nums(&[1.0, 2.0, 3.0])); // length mismatch
    let nested_a = Value::from(vec![nums(&[1.0]), s("x")]);
    let nested_b = Value::from(vec![nums(&[1.0]), s("x")]);
    assert_eq!(nested_a, nested_b);
}

#[test]
fn range_equality_uses_num_values() {
    assert_eq!(range(0.0, 1.0, 10.0), range(0.0, 1.0, 10.0));
    // Upstream RangeType::operator==: two empty ranges are equal regardless of fields.
    assert_eq!(range(10.0, 1.0, 0.0), range(5.0, 2.0, 3.0));
    assert_ne!(range(10.0, 1.0, 0.0), range(0.0, 1.0, 10.0));
    // Same begin, same step, same numValues() — equal even though end differs.
    assert_eq!(range(0.0, 1.0, 10.0), range(0.0, 1.0, 10.5));
}

// ---------------------------------------------------------------------------
// Ordering — upstream less/greater visitors: mismatched types yield undef
// ---------------------------------------------------------------------------

#[test]
fn number_ordering() {
    assert_eq!(n(1.0).lt(&n(2.0)), Value::Bool(true));
    assert_eq!(n(2.0).lt(&n(1.0)), Value::Bool(false));
    assert_eq!(n(1.0).le(&n(1.0)), Value::Bool(true));
    assert_eq!(n(2.0).gt(&n(1.0)), Value::Bool(true));
    assert_eq!(n(1.0).ge(&n(2.0)), Value::Bool(false));
}

#[test]
fn nan_ordering_is_false_not_undef() {
    // Same-type double comparison: IEEE, all comparisons with NaN are false.
    assert_eq!(n(f64::NAN).lt(&n(1.0)), Value::Bool(false));
    assert_eq!(n(f64::NAN).ge(&n(1.0)), Value::Bool(false));
}

#[test]
fn string_ordering_is_lexicographic() {
    assert_eq!(s("a").lt(&s("b")), Value::Bool(true));
    assert_eq!(s("abc").lt(&s("abd")), Value::Bool(true));
    assert_eq!(s("").lt(&s("a")), Value::Bool(true));
    assert_eq!(s("b").ge(&s("a")), Value::Bool(true));
}

#[test]
fn bool_ordering_false_lt_true() {
    assert_eq!(Value::Bool(false).lt(&Value::Bool(true)), Value::Bool(true));
    assert_eq!(
        Value::Bool(true).lt(&Value::Bool(false)),
        Value::Bool(false)
    );
    assert_eq!(Value::Bool(true).ge(&Value::Bool(false)), Value::Bool(true));
}

#[test]
fn mixed_type_ordering_is_undef() {
    let r = n(1.0).lt(&s("a"));
    assert!(r.is_undef());
    assert_eq!(
        r.undef_message(),
        Some("undefined operation (number < string)")
    );
    assert!(s("a").gt(&nums(&[1.0])).is_undef());
    assert!(Value::Bool(true).le(&n(1.0)).is_undef());
}

#[test]
fn undef_ordering_undef_is_undef() {
    // Upstream UndefType::operator< — note the "operation undefined" word order.
    let r = undef().lt(&undef());
    assert!(r.is_undef());
    assert_eq!(
        r.undef_message(),
        Some("operation undefined (undefined < undefined)")
    );
    assert!(undef().ge(&undef()).is_undef());
}

#[test]
fn vector_ordering_is_lexicographic() {
    assert_eq!(nums(&[1.0, 2.0]).lt(&nums(&[1.0, 3.0])), Value::Bool(true));
    // Shorter prefix sorts first.
    assert_eq!(nums(&[1.0]).lt(&nums(&[1.0, 0.0])), Value::Bool(true));
    assert_eq!(nums(&[2.0]).gt(&nums(&[1.0, 99.0])), Value::Bool(true));
    assert_eq!(nums(&[1.0, 2.0]).lt(&nums(&[1.0, 2.0])), Value::Bool(false));
    assert_eq!(nums(&[1.0, 2.0]).le(&nums(&[1.0, 2.0])), Value::Bool(true));
    assert_eq!(nums(&[1.0, 2.0]).ge(&nums(&[1.0, 2.0])), Value::Bool(true));
}

#[test]
fn vector_ordering_with_mismatched_elements_is_undef() {
    let r = nums(&[1.0]).lt(&Value::from(vec![s("a")]));
    assert!(r.is_undef());
    // Upstream appends "in vector comparison at index i" to the element's reason.
    let msg = r.undef_message().unwrap();
    assert!(msg.contains("undefined operation (number < string)"));
    assert!(msg.contains("in vector comparison at index 0"));
}

#[test]
fn range_ordering() {
    assert_eq!(
        range(0.0, 1.0, 5.0).lt(&range(1.0, 1.0, 5.0)),
        Value::Bool(true)
    );
    // Empty range sorts before any non-empty range.
    assert_eq!(
        range(10.0, 1.0, 0.0).lt(&range(0.0, 1.0, 5.0)),
        Value::Bool(true)
    );
    assert_eq!(
        range(0.0, 1.0, 5.0).le(&range(0.0, 1.0, 5.0)),
        Value::Bool(true)
    );
}

// ---------------------------------------------------------------------------
// Addition / subtraction — upstream plus_visitor / minus_visitor
// ---------------------------------------------------------------------------

#[test]
fn number_add_sub() {
    assert_eq!(n(1.5).add(&n(2.5)), n(4.0));
    assert_eq!(n(1.0).sub(&n(3.0)), n(-2.0));
}

#[test]
fn vector_add_is_elementwise() {
    assert_eq!(
        nums(&[1.0, 2.0]).add(&nums(&[10.0, 20.0])),
        nums(&[11.0, 22.0])
    );
}

#[test]
fn vector_add_truncates_to_shortest() {
    // Upstream plus_visitor iterates while BOTH iterators are valid (with an
    // explicit FIXME acknowledging the truncation) — result has min length.
    assert_eq!(
        nums(&[1.0, 2.0, 3.0]).add(&nums(&[10.0, 20.0])),
        nums(&[11.0, 22.0])
    );
    assert_eq!(
        nums(&[5.0, 5.0]).sub(&nums(&[1.0, 2.0, 3.0])),
        nums(&[4.0, 3.0])
    );
}

#[test]
fn nested_vector_add_recurses() {
    let a = Value::from(vec![nums(&[1.0, 2.0])]);
    let b = Value::from(vec![nums(&[10.0, 20.0])]);
    assert_eq!(a.add(&b), Value::from(vec![nums(&[11.0, 22.0])]));
}

#[test]
fn add_type_mismatches_are_undef() {
    let r = n(1.0).add(&s("a"));
    assert!(r.is_undef());
    assert_eq!(
        r.undef_message(),
        Some("undefined operation (number + string)")
    );
    // No string concatenation with + in OpenSCAD.
    assert!(s("a").add(&s("b")).is_undef());
    assert!(Value::Bool(true).add(&Value::Bool(true)).is_undef());
    assert!(undef().add(&n(1.0)).is_undef());
    assert!(n(1.0).sub(&s("a")).is_undef());
}

// ---------------------------------------------------------------------------
// Multiplication — upstream multiply_visitor
// ---------------------------------------------------------------------------

#[test]
fn number_mul() {
    assert_eq!(n(3.0).mul(&n(4.0)), n(12.0));
}

#[test]
fn scalar_vector_mul_scales_recursively() {
    assert_eq!(n(2.0).mul(&nums(&[1.0, 2.0])), nums(&[2.0, 4.0]));
    assert_eq!(nums(&[1.0, 2.0]).mul(&n(2.0)), nums(&[2.0, 4.0]));
    // Nested vectors scale recursively (multvecnum multiplies each element).
    let m = Value::from(vec![nums(&[1.0]), nums(&[2.0])]);
    assert_eq!(
        n(2.0).mul(&m),
        Value::from(vec![nums(&[2.0]), nums(&[4.0])])
    );
}

#[test]
fn vector_vector_mul_is_dot_product() {
    assert_eq!(nums(&[1.0, 2.0, 3.0]).mul(&nums(&[4.0, 5.0, 6.0])), n(32.0));
}

#[test]
fn dot_product_length_mismatch_is_undef() {
    // Upstream requires matching lengths for vector*vector.
    assert!(nums(&[1.0, 2.0]).mul(&nums(&[1.0, 2.0, 3.0])).is_undef());
}

#[test]
fn dot_product_non_number_element_is_undef() {
    let a = Value::from(vec![n(1.0), s("x")]);
    assert!(a.mul(&nums(&[1.0, 2.0])).is_undef());
}

#[test]
fn matrix_vector_mul() {
    let m = Value::from(vec![nums(&[1.0, 2.0]), nums(&[3.0, 4.0])]);
    assert_eq!(m.mul(&nums(&[5.0, 6.0])), nums(&[17.0, 39.0]));
}

#[test]
fn vector_matrix_mul() {
    let m = Value::from(vec![nums(&[1.0, 2.0]), nums(&[3.0, 4.0])]);
    assert_eq!(nums(&[5.0, 6.0]).mul(&m), nums(&[23.0, 34.0]));
}

#[test]
fn matrix_matrix_mul() {
    let a = Value::from(vec![nums(&[1.0, 2.0]), nums(&[3.0, 4.0])]);
    let b = Value::from(vec![nums(&[5.0, 6.0]), nums(&[7.0, 8.0])]);
    assert_eq!(
        a.mul(&b),
        Value::from(vec![nums(&[19.0, 22.0]), nums(&[43.0, 50.0])])
    );
}

#[test]
fn matrix_matrix_mul_non_square() {
    // (2x3) * (3x1) = (2x1)
    let a = Value::from(vec![nums(&[1.0, 2.0, 3.0]), nums(&[4.0, 5.0, 6.0])]);
    let b = Value::from(vec![nums(&[7.0]), nums(&[8.0]), nums(&[9.0])]);
    assert_eq!(a.mul(&b), Value::from(vec![nums(&[50.0]), nums(&[122.0])]));
}

#[test]
fn matrix_mul_dimension_mismatch_is_undef() {
    let m = Value::from(vec![nums(&[1.0, 2.0])]);
    assert!(m.mul(&nums(&[5.0])).is_undef()); // matrix cols 2 != vec len 1
                                              // vector*matrix: vec len 2 != matrix row count 1.
    assert!(nums(&[5.0, 6.0]).mul(&m).is_undef());
    // matrix*matrix: left cols 2 != right rows 3.
    let c = Value::from(vec![nums(&[1.0]), nums(&[2.0]), nums(&[3.0])]);
    assert!(m.mul(&c).is_undef());
}

#[test]
fn empty_vector_mul_is_undef() {
    let r = Value::from(vec![]).mul(&nums(&[1.0]));
    assert!(r.is_undef());
    assert_eq!(
        r.undef_message(),
        Some("Multiplication is undefined on empty vectors")
    );
}

#[test]
fn string_mul_is_undef() {
    assert!(s("ab").mul(&n(2.0)).is_undef());
    assert!(n(2.0).mul(&s("ab")).is_undef());
}

// ---------------------------------------------------------------------------
// Division / modulo / power — upstream operator/ operator% operator^
// ---------------------------------------------------------------------------

#[test]
fn number_div() {
    assert_eq!(n(7.0).div(&n(2.0)), n(3.5));
    assert_eq!(n(1.0).div(&n(0.0)), n(f64::INFINITY)); // IEEE, as upstream
    assert_eq!(n(-1.0).div(&n(0.0)), n(f64::NEG_INFINITY));
    let z = n(0.0).div(&n(0.0));
    match z {
        Value::Number(x) => assert!(x.is_nan()),
        other => panic!("expected NaN number, got {other:?}"),
    }
}

#[test]
fn vector_scalar_div_is_elementwise() {
    assert_eq!(nums(&[2.0, 4.0]).div(&n(2.0)), nums(&[1.0, 2.0]));
}

#[test]
fn scalar_vector_div_is_elementwise() {
    // Upstream operator/ explicitly supports number/vector: divides the
    // number by each element.
    assert_eq!(n(12.0).div(&nums(&[3.0, 4.0])), nums(&[4.0, 3.0]));
}

#[test]
fn div_mismatches_are_undef() {
    assert!(s("a").div(&n(2.0)).is_undef());
    // vector/vector is NOT defined upstream.
    assert!(nums(&[1.0]).div(&nums(&[2.0])).is_undef());
    assert!(Value::Bool(true).div(&n(2.0)).is_undef());
}

#[test]
fn modulo_is_fmod() {
    assert_eq!(n(7.0).modulo(&n(3.0)), n(1.0));
    assert_eq!(n(-7.0).modulo(&n(3.0)), n(-1.0)); // fmod: sign of dividend
    assert_eq!(n(7.0).modulo(&n(-3.0)), n(1.0));
    assert_eq!(n(7.5).modulo(&n(2.0)), n(1.5));
    match n(1.0).modulo(&n(0.0)) {
        Value::Number(x) => assert!(x.is_nan()),
        other => panic!("expected NaN number, got {other:?}"),
    }
}

#[test]
fn modulo_mismatch_is_undef() {
    assert!(nums(&[1.0]).modulo(&n(2.0)).is_undef());
    assert!(s("a").modulo(&s("b")).is_undef());
}

#[test]
fn pow_numbers() {
    assert_eq!(n(2.0).pow(&n(10.0)), n(1024.0));
    assert_eq!(n(2.0).pow(&n(-1.0)), n(0.5));
}

#[test]
fn pow_mismatch_is_undef() {
    let r = s("a").pow(&n(2.0));
    assert!(r.is_undef());
    assert_eq!(
        r.undef_message(),
        Some("undefined operation (string ^ number)")
    );
    assert!(nums(&[2.0]).pow(&n(2.0)).is_undef());
}

// ---------------------------------------------------------------------------
// Unary — upstream operator- ; logical not via toBool
// ---------------------------------------------------------------------------

#[test]
fn neg_number_and_vector() {
    assert_eq!(n(5.0).neg(), n(-5.0));
    assert_eq!(nums(&[1.0, -2.0]).neg(), nums(&[-1.0, 2.0]));
    // Recursive on nested vectors.
    let m = Value::from(vec![nums(&[1.0]), nums(&[2.0])]);
    assert_eq!(m.neg(), Value::from(vec![nums(&[-1.0]), nums(&[-2.0])]));
}

#[test]
fn neg_mismatch_is_undef() {
    let r = s("a").neg();
    assert!(r.is_undef());
    assert_eq!(r.undef_message(), Some("undefined operation (-string)"));
    assert!(Value::Bool(true).neg().is_undef());
    assert!(undef().neg().is_undef());
}

#[test]
fn logical_not_uses_truthiness() {
    assert_eq!(n(0.0).not(), Value::Bool(true));
    assert_eq!(n(1.0).not(), Value::Bool(false));
    assert_eq!(undef().not(), Value::Bool(true));
    assert_eq!(s("").not(), Value::Bool(true));
    assert_eq!(nums(&[1.0]).not(), Value::Bool(false));
}

// ---------------------------------------------------------------------------
// Display — upstream toString()/tostream_visitor + DoubleConvert
// ---------------------------------------------------------------------------

#[test]
fn display_integers_without_decimals() {
    assert_eq!(n(5.0).to_string(), "5");
    assert_eq!(n(-5.0).to_string(), "-5");
    assert_eq!(n(0.0).to_string(), "0");
    assert_eq!(n(-0.0).to_string(), "0"); // UNIQUE_ZERO flag upstream
    assert_eq!(n(100000.0).to_string(), "100000");
    assert_eq!(n(100.0).to_string(), "100");
}

#[test]
fn display_floats_trim_trailing_zeroes() {
    assert_eq!(n(0.5).to_string(), "0.5");
    assert_eq!(n(0.1 + 0.2).to_string(), "0.3"); // 6-sig-digit rounding hides FP noise
    assert_eq!(n(2.5).to_string(), "2.5");
}

#[test]
fn display_rounds_to_six_significant_digits() {
    // Upstream DC_PRECISION_REQUESTED == 6.
    assert_eq!(n(1.0 / 3.0).to_string(), "0.333333");
    assert_eq!(n(123456.7).to_string(), "123457");
}

#[test]
fn display_exponential_thresholds() {
    // decimal form only while decimal point stays within [-4, 6] (upstream
    // DC_MAX_LEADING_ZEROES=5, DC_MAX_TRAILING_ZEROES=0), with '+' emitted
    // for positive exponents (EMIT_POSITIVE_EXPONENT_SIGN).
    assert_eq!(n(1_000_000.0).to_string(), "1e+6");
    assert_eq!(n(1_500_000.0).to_string(), "1.5e+6");
    assert_eq!(n(0.00001).to_string(), "0.00001");
    assert_eq!(n(0.000001).to_string(), "1e-6");
    assert_eq!(n(0.0000001).to_string(), "1e-7");
}

#[test]
fn display_special_numbers() {
    assert_eq!(n(f64::NAN).to_string(), "nan");
    assert_eq!(n(f64::INFINITY).to_string(), "inf");
    assert_eq!(n(f64::NEG_INFINITY).to_string(), "-inf");
}

#[test]
fn display_bool_and_undef() {
    assert_eq!(Value::Bool(true).to_string(), "true");
    assert_eq!(Value::Bool(false).to_string(), "false");
    assert_eq!(undef().to_string(), "undef");
    assert_eq!(Value::undef("why").to_string(), "undef"); // message not part of str()
}

#[test]
fn display_string_top_level_is_unquoted() {
    assert_eq!(s("hello").to_string(), "hello");
}

#[test]
fn display_vector() {
    assert_eq!(nums(&[1.0, 2.0, 3.0]).to_string(), "[1, 2, 3]");
    assert_eq!(Value::from(vec![]).to_string(), "[]");
    let nested = Value::from(vec![nums(&[1.0, 2.0]), Value::from(vec![])]);
    assert_eq!(nested.to_string(), "[[1, 2], []]");
    // Strings inside vectors are quoted (tostream_visitor).
    let mixed = Value::from(vec![s("a"), n(1.0), Value::Bool(true), undef()]);
    assert_eq!(mixed.to_string(), "[\"a\", 1, true, undef]");
}

#[test]
fn display_range() {
    assert_eq!(range(0.0, 1.0, 10.0).to_string(), "[0 : 1 : 10]");
    assert_eq!(range(0.0, 0.5, 2.0).to_string(), "[0 : 0.5 : 2]");
    assert_eq!(range(10.0, -2.0, 0.0).to_string(), "[10 : -2 : 0]");
}

#[test]
fn format_number_is_exposed() {
    assert_eq!(format_number(42.0), "42");
    assert_eq!(format_number(0.25), "0.25");
}

// ---------------------------------------------------------------------------
// Range iteration — upstream RangeType (RangeType.h)
// ---------------------------------------------------------------------------

#[test]
fn range_iter_basic() {
    let r = Range::new(0.0, 1.0, 10.0);
    let vals: Vec<f64> = r.iter().collect();
    assert_eq!(vals.len(), 11);
    assert_eq!(vals[0], 0.0);
    assert_eq!(vals[10], 10.0);
}

#[test]
fn range_iter_fractional_step_uses_nextafter_compensation() {
    // [1 : 0.1 : 1.3] must yield 4 values despite FP inaccuracy.
    let vals: Vec<f64> = Range::new(1.0, 0.1, 1.3).iter().collect();
    assert_eq!(vals.len(), 4);
    assert!((vals[3] - 1.3).abs() < 1e-9);
}

#[test]
fn range_iter_negative_step_counts_down() {
    let vals: Vec<f64> = Range::new(10.0, -2.0, 0.0).iter().collect();
    assert_eq!(vals, vec![10.0, 8.0, 6.0, 4.0, 2.0, 0.0]);
}

#[test]
fn range_iter_wrong_direction_is_empty() {
    assert_eq!(Range::new(10.0, 1.0, 0.0).iter().count(), 0);
    assert_eq!(Range::new(0.0, -1.0, 10.0).iter().count(), 0);
}

#[test]
fn range_iter_zero_step_is_empty_but_num_values_saturates() {
    // Upstream: numValues() returns uint32 max for step==0 (potentially
    // infinite), but the iterator constructor bails out immediately.
    let r = Range::new(0.0, 0.0, 10.0);
    assert_eq!(r.iter().count(), 0);
    assert_eq!(r.num_values(), u32::MAX);
}

#[test]
fn range_iter_nan_is_empty() {
    assert_eq!(Range::new(f64::NAN, 1.0, 10.0).iter().count(), 0);
    assert_eq!(Range::new(0.0, f64::NAN, 10.0).iter().count(), 0);
    assert_eq!(Range::new(0.0, 1.0, f64::NAN).iter().count(), 0);
    assert_eq!(Range::new(f64::NAN, 1.0, 10.0).num_values(), 0);
}

#[test]
fn range_single_value_cases() {
    // begin == end -> exactly one value.
    let vals: Vec<f64> = Range::new(5.0, 1.0, 5.0).iter().collect();
    assert_eq!(vals, vec![5.0]);
    // Infinite step -> one value.
    assert_eq!(Range::new(0.0, f64::INFINITY, 10.0).num_values(), 1);
}

#[test]
fn range_num_values() {
    assert_eq!(Range::new(0.0, 1.0, 10.0).num_values(), 11);
    assert_eq!(Range::new(10.0, 1.0, 0.0).num_values(), 0);
    assert_eq!(Range::new(10.0, -2.0, 0.0).num_values(), 6);
    assert_eq!(Range::new(1.0, 0.1, 1.3).num_values(), 4);
}

// ---------------------------------------------------------------------------
// Indexing — upstream bracket_visitor + str_utf8_wrapper::operator[]
// ---------------------------------------------------------------------------

#[test]
fn vector_indexing() {
    let v = nums(&[10.0, 20.0, 30.0]);
    assert_eq!(v.index(&n(0.0)), n(10.0));
    assert_eq!(v.index(&n(2.0)), n(30.0));
}

#[test]
fn vector_index_out_of_bounds_is_undef() {
    let v = nums(&[10.0, 20.0, 30.0]);
    let r = v.index(&n(3.0));
    assert!(r.is_undef());
    assert_eq!(
        r.undef_message(),
        Some("index 3 out of bounds for vector of size 3")
    );
    assert!(v.index(&n(-1.0)).is_undef()); // negative -> uint32 max -> OOB
}

#[test]
fn vector_fractional_index_truncates() {
    // boost::numeric_cast default Float2IntRounder is Trunc.
    let v = nums(&[10.0, 20.0, 30.0]);
    assert_eq!(v.index(&n(1.5)), n(20.0));
    // boost::numeric_cast Trunc accepts (-1.0, 0.0): truncates to 0.
    assert_eq!(v.index(&n(-0.5)), n(10.0));
    assert!(v.index(&n(-1.0)).is_undef());
}

#[test]
fn string_indexing_ascii() {
    assert_eq!(s("abc").index(&n(0.0)), s("a"));
    assert_eq!(s("abc").index(&n(2.0)), s("c"));
}

#[test]
fn string_indexing_is_by_code_point_not_byte() {
    // Upstream str_utf8_wrapper::operator[] offsets by UTF-8 character.
    assert_eq!(s("héllo").index(&n(1.0)), s("é"));
    assert_eq!(s("日本語").index(&n(2.0)), s("語"));
    assert_eq!(s("a😀b").index(&n(1.0)), s("😀"));
    assert_eq!(s("a😀b").index(&n(2.0)), s("b"));
}

#[test]
fn string_index_out_of_bounds_is_plain_undef() {
    // Upstream returns Value::undefined.clone() here — no reason message.
    let r = s("abc").index(&n(3.0));
    assert!(r.is_undef());
    assert_eq!(r.undef_message(), None);
    assert!(s("abc").index(&n(-1.0)).is_undef());
    assert!(s("").index(&n(0.0)).is_undef());
}

#[test]
fn range_indexing_yields_begin_step_end() {
    let r = range(0.0, 1.0, 10.0);
    assert_eq!(r.index(&n(0.0)), n(0.0));
    assert_eq!(r.index(&n(1.0)), n(1.0));
    assert_eq!(r.index(&n(2.0)), n(10.0));
    assert!(r.index(&n(3.0)).is_undef());
}

#[test]
fn index_type_mismatches_are_undef() {
    let r = n(5.0).index(&n(0.0));
    assert!(r.is_undef());
    assert_eq!(
        r.undef_message(),
        Some("undefined operation number[number]")
    );
    let r = nums(&[1.0]).index(&s("a"));
    assert!(r.is_undef());
    assert_eq!(
        r.undef_message(),
        Some("undefined operation vector[string]")
    );
    assert!(undef().index(&n(0.0)).is_undef());
}

// ---------------------------------------------------------------------------
// Misc surface
// ---------------------------------------------------------------------------

#[test]
fn type_names_match_upstream() {
    assert_eq!(undef().type_name(), "undefined");
    assert_eq!(Value::Bool(true).type_name(), "bool");
    assert_eq!(n(1.0).type_name(), "number");
    assert_eq!(s("").type_name(), "string");
    assert_eq!(Value::from(vec![]).type_name(), "vector");
    assert_eq!(range(0.0, 1.0, 2.0).type_name(), "range");
}
