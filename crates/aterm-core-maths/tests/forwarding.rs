// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Every [`CoreFloat`] method, called once on `f32` and once on `f64`.
//!
//! Two jobs, and the first is the one that could not be done by reading:
//!
//! 1. **Recursion.** Each body in `src/lib.rs` is written `<$t>::name(self)`,
//!    which resolves to the inherent `std` method — but the same body written
//!    `self.name()` would compile just as cleanly and call `CoreFloat::name`
//!    forever. Calling all 38 methods on both types turns any such body into a
//!    stack overflow here rather than a hang in a shipped binary.
//! 2. **The exactly-specified subset is exact.** `floor`, `ceil`, `round`,
//!    `trunc`, `fract`, `abs`, `copysign`, `sqrt` and `mul_add` have ONE
//!    correct answer per input under IEEE 754, so `std` and the `libm` this
//!    crate replaces must agree bit for bit. Those are pinned as exact
//!    equalities below. The transcendentals are not: they are checked against a
//!    tolerance, because pinning a bit pattern for `sin` would encode one
//!    platform's libm into a test that runs on three.

use core_maths::CoreFloat;

/// Half an ulp of nothing — a tolerance loose enough for any conforming libm
/// and tight enough that a wrong FUNCTION (rather than a wrong last bit) fails.
const EPS32: f32 = 1e-5;
const EPS64: f64 = 1e-12;

macro_rules! near {
    ($got:expr, $want:expr, $eps:expr) => {{
        let (got, want) = ($got, $want);
        assert!(
            (got - want).abs() <= $eps,
            "{} = {got}, expected ~{want}",
            stringify!($got)
        );
    }};
}

#[test]
fn f32_rounding_and_sign_are_bit_exact() {
    assert_eq!(CoreFloat::floor(3.7_f32), 3.0);
    assert_eq!(CoreFloat::floor(-3.7_f32), -4.0);
    assert_eq!(CoreFloat::ceil(3.01_f32), 4.0);
    assert_eq!(CoreFloat::ceil(-3.01_f32), -3.0);
    // Halfway cases round AWAY FROM ZERO — the clause that separates `round`
    // from `rint`, and the only CoreFloat method rustybuzz would call if its
    // `std` feature were ever off.
    assert_eq!(CoreFloat::round(3.5_f32), 4.0);
    assert_eq!(CoreFloat::round(4.5_f32), 5.0);
    assert_eq!(CoreFloat::round(-3.5_f32), -4.0);
    assert_eq!(CoreFloat::round(-0.5_f32), -1.0);
    assert_eq!(CoreFloat::trunc(3.7_f32), 3.0);
    assert_eq!(CoreFloat::trunc(-3.7_f32), -3.0);
    assert_eq!(CoreFloat::fract(3.5_f32), 0.5);
    assert_eq!(CoreFloat::abs(-3.5_f32), 3.5);
    assert_eq!(CoreFloat::signum(-3.5_f32), -1.0);
    assert!(CoreFloat::signum(f32::NAN).is_nan());
    assert_eq!(CoreFloat::copysign(3.5_f32, -1.0), -3.5);
    // `sqrt` and `mul_add` are exactly-rounded IEEE operations, so these are
    // equalities and not tolerances on purpose.
    assert_eq!(CoreFloat::sqrt(16.0_f32), 4.0);
    assert_eq!(CoreFloat::mul_add(2.0_f32, 3.0, 4.0), 10.0);
    assert_eq!(CoreFloat::div_euclid(7.0_f32, 4.0), 1.0);
    assert_eq!(CoreFloat::div_euclid(-7.0_f32, 4.0), -2.0);
    assert_eq!(CoreFloat::rem_euclid(-7.0_f32, 4.0), 1.0);
}

#[test]
fn f32_powi_matches_upstreams_own_loop() {
    // Reproduced from upstream rather than forwarded to `<f32>::powi`, so the
    // identities it must satisfy are checked here.
    assert_eq!(CoreFloat::powi(2.0_f32, 0), 1.0);
    assert_eq!(CoreFloat::powi(2.0_f32, 1), 2.0);
    assert_eq!(CoreFloat::powi(2.0_f32, 10), 1024.0);
    assert_eq!(CoreFloat::powi(2.0_f32, -2), 0.25);
    assert_eq!(CoreFloat::powi(-3.0_f32, 3), -27.0);
    assert_eq!(CoreFloat::powi(0.0_f32, 0), 1.0);
}

#[test]
fn f32_transcendentals_answer_the_right_function() {
    near!(CoreFloat::powf(2.0_f32, 10.0), 1024.0, 1e-2);
    near!(CoreFloat::exp(0.0_f32), 1.0, EPS32);
    near!(CoreFloat::exp2(10.0_f32), 1024.0, 1e-2);
    near!(CoreFloat::ln(core::f32::consts::E), 1.0, EPS32);
    near!(CoreFloat::log(8.0_f32, 2.0), 3.0, 1e-4);
    near!(CoreFloat::log2(1024.0_f32), 10.0, 1e-4);
    near!(CoreFloat::log10(1000.0_f32), 3.0, 1e-4);
    near!(CoreFloat::cbrt(27.0_f32), 3.0, 1e-4);
    near!(CoreFloat::hypot(3.0_f32, 4.0), 5.0, EPS32);
    near!(CoreFloat::sin(0.0_f32), 0.0, EPS32);
    near!(CoreFloat::cos(0.0_f32), 1.0, EPS32);
    near!(CoreFloat::tan(0.0_f32), 0.0, EPS32);
    near!(
        CoreFloat::asin(1.0_f32),
        core::f32::consts::FRAC_PI_2,
        EPS32
    );
    near!(CoreFloat::acos(1.0_f32), 0.0, EPS32);
    near!(CoreFloat::atan(0.0_f32), 0.0, EPS32);
    near!(
        CoreFloat::atan2(1.0_f32, 1.0),
        core::f32::consts::FRAC_PI_4,
        EPS32
    );
    let (s, c) = CoreFloat::sin_cos(0.0_f32);
    near!(s, 0.0, EPS32);
    near!(c, 1.0, EPS32);
    near!(CoreFloat::exp_m1(0.0_f32), 0.0, EPS32);
    near!(CoreFloat::ln_1p(0.0_f32), 0.0, EPS32);
    near!(CoreFloat::sinh(0.0_f32), 0.0, EPS32);
    near!(CoreFloat::cosh(0.0_f32), 1.0, EPS32);
    near!(CoreFloat::tanh(0.0_f32), 0.0, EPS32);
    near!(CoreFloat::asinh(0.0_f32), 0.0, EPS32);
    near!(CoreFloat::acosh(1.0_f32), 0.0, EPS32);
    near!(CoreFloat::atanh(0.0_f32), 0.0, EPS32);
}

#[test]
fn f64_rounding_and_sign_are_bit_exact() {
    assert_eq!(CoreFloat::floor(3.7_f64), 3.0);
    assert_eq!(CoreFloat::floor(-3.7_f64), -4.0);
    assert_eq!(CoreFloat::ceil(3.01_f64), 4.0);
    assert_eq!(CoreFloat::ceil(-3.01_f64), -3.0);
    assert_eq!(CoreFloat::round(3.5_f64), 4.0);
    assert_eq!(CoreFloat::round(4.5_f64), 5.0);
    assert_eq!(CoreFloat::round(-3.5_f64), -4.0);
    assert_eq!(CoreFloat::round(-0.5_f64), -1.0);
    assert_eq!(CoreFloat::trunc(3.7_f64), 3.0);
    assert_eq!(CoreFloat::trunc(-3.7_f64), -3.0);
    assert_eq!(CoreFloat::fract(3.5_f64), 0.5);
    assert_eq!(CoreFloat::abs(-3.5_f64), 3.5);
    assert_eq!(CoreFloat::signum(-3.5_f64), -1.0);
    assert!(CoreFloat::signum(f64::NAN).is_nan());
    assert_eq!(CoreFloat::copysign(3.5_f64, -1.0), -3.5);
    assert_eq!(CoreFloat::sqrt(16.0_f64), 4.0);
    assert_eq!(CoreFloat::mul_add(2.0_f64, 3.0, 4.0), 10.0);
    assert_eq!(CoreFloat::div_euclid(7.0_f64, 4.0), 1.0);
    assert_eq!(CoreFloat::div_euclid(-7.0_f64, 4.0), -2.0);
    assert_eq!(CoreFloat::rem_euclid(-7.0_f64, 4.0), 1.0);
}

#[test]
fn f64_powi_matches_upstreams_own_loop() {
    assert_eq!(CoreFloat::powi(2.0_f64, 0), 1.0);
    assert_eq!(CoreFloat::powi(2.0_f64, 1), 2.0);
    assert_eq!(CoreFloat::powi(2.0_f64, 10), 1024.0);
    assert_eq!(CoreFloat::powi(2.0_f64, -2), 0.25);
    assert_eq!(CoreFloat::powi(-3.0_f64, 3), -27.0);
    assert_eq!(CoreFloat::powi(0.0_f64, 0), 1.0);
}

#[test]
fn f64_transcendentals_answer_the_right_function() {
    near!(CoreFloat::powf(2.0_f64, 10.0), 1024.0, 1e-9);
    near!(CoreFloat::exp(0.0_f64), 1.0, EPS64);
    near!(CoreFloat::exp2(10.0_f64), 1024.0, 1e-9);
    near!(CoreFloat::ln(core::f64::consts::E), 1.0, EPS64);
    near!(CoreFloat::log(8.0_f64, 2.0), 3.0, 1e-12);
    near!(CoreFloat::log2(1024.0_f64), 10.0, 1e-12);
    near!(CoreFloat::log10(1000.0_f64), 3.0, 1e-12);
    near!(CoreFloat::cbrt(27.0_f64), 3.0, 1e-12);
    near!(CoreFloat::hypot(3.0_f64, 4.0), 5.0, EPS64);
    near!(CoreFloat::sin(0.0_f64), 0.0, EPS64);
    near!(CoreFloat::cos(0.0_f64), 1.0, EPS64);
    near!(CoreFloat::tan(0.0_f64), 0.0, EPS64);
    near!(
        CoreFloat::asin(1.0_f64),
        core::f64::consts::FRAC_PI_2,
        EPS64
    );
    near!(CoreFloat::acos(1.0_f64), 0.0, EPS64);
    near!(CoreFloat::atan(0.0_f64), 0.0, EPS64);
    near!(
        CoreFloat::atan2(1.0_f64, 1.0),
        core::f64::consts::FRAC_PI_4,
        EPS64
    );
    let (s, c) = CoreFloat::sin_cos(0.0_f64);
    near!(s, 0.0, EPS64);
    near!(c, 1.0, EPS64);
    near!(CoreFloat::exp_m1(0.0_f64), 0.0, EPS64);
    near!(CoreFloat::ln_1p(0.0_f64), 0.0, EPS64);
    near!(CoreFloat::sinh(0.0_f64), 0.0, EPS64);
    near!(CoreFloat::cosh(0.0_f64), 1.0, EPS64);
    near!(CoreFloat::tanh(0.0_f64), 0.0, EPS64);
    near!(CoreFloat::asinh(0.0_f64), 0.0, EPS64);
    near!(CoreFloat::acosh(1.0_f64), 0.0, EPS64);
    near!(CoreFloat::atanh(0.0_f64), 0.0, EPS64);
}
