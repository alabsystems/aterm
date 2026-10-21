// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-party `libm` facade wired via `[patch.crates-io]`, forwarding the
//! 68 `f32`/`f64` math functions used by `num-traits` (66 functions in
//! `float_impl_libm!` / `FloatCore`) and `naga` (`asinh`, `rint`, `rintf`)
//! to `std`.
//!
//! # Fidelity
//!
//! * **Exact IEEE operations**: `floor`, `ceil`, `round`, `trunc`, `fabs`,
//!   `copysign`, `sqrt`, `fma`, `rint`, and `fdim`.
//! * **Transcendentals**: forwarded to `std` (platform libm on native,
//!   `compiler-builtins` musl libm on `wasm32-unknown-unknown`). `atanh` and
//!   `atanhf` evaluate `std`'s `ln_1p` formula on `x.abs()` and restore the
//!   sign to avoid cancellation near `-1`. Verified against recorded musl
//!   outputs in `tests/recorded.rs`.
//!
//! Requires `std` (present on all aterm targets, including
//! `wasm32-unknown-unknown`); upstream feature flags (`arch`,
//! `force-soft-floats`, `unstable*`) are accepted as no-ops.

#![warn(missing_docs)]

/// Forward a libm function pair (`f64` name, `f32` name) to one std method.
///
/// The bodies call `f64::method(..)` by path, never a bare name: the crate
/// root defines functions called `sin`, `floor` and so on, and a bare call
/// would recurse into itself.
macro_rules! forward {
    ($($what:literal: $f64:ident / $f32:ident ($($a:ident),+) => $std:ident;)*) => {$(
        #[doc = concat!($what, " (`f64`); forwards to `f64::", stringify!($std), "`.")]
        #[inline]
        #[must_use]
        pub fn $f64($($a: f64),+) -> f64 {
            f64::$std($($a),+)
        }

        #[doc = concat!($what, " (`f32`); forwards to `f32::", stringify!($std), "`.")]
        #[inline]
        #[must_use]
        pub fn $f32($($a: f32),+) -> f32 {
            f32::$std($($a),+)
        }
    )*};
}

forward! {
    "Largest integer not greater than `x`": floor / floorf (x) => floor;
    "Smallest integer not less than `x`": ceil / ceilf (x) => ceil;
    "Nearest integer, halfway cases away from zero": round / roundf (x) => round;
    "Integer part of `x`, toward zero": trunc / truncf (x) => trunc;
    "Absolute value": fabs / fabsf (x) => abs;
    "Nearest integer, halfway cases to even (the default rounding mode)":
        rint / rintf (x) => round_ties_even;
    "`y`'s sign on `x`'s magnitude": copysign / copysignf (x, y) => copysign;
    "Square root": sqrt / sqrtf (x) => sqrt;
    "`x * y + z` with a single rounding": fma / fmaf (x, y, z) => mul_add;
    "`x` raised to `y`": pow / powf (x, y) => powf;
    "`e^x`": exp / expf (x) => exp;
    "`2^x`": exp2 / exp2f (x) => exp2;
    "`e^x - 1`, accurate near zero": expm1 / expm1f (x) => exp_m1;
    "Natural logarithm": log / logf (x) => ln;
    "Base-2 logarithm": log2 / log2f (x) => log2;
    "Base-10 logarithm": log10 / log10f (x) => log10;
    "`ln(1 + x)`, accurate near zero": log1p / log1pf (x) => ln_1p;
    "Cube root": cbrt / cbrtf (x) => cbrt;
    "`sqrt(x^2 + y^2)` without undue overflow": hypot / hypotf (x, y) => hypot;
    "Sine (radians)": sin / sinf (x) => sin;
    "Cosine (radians)": cos / cosf (x) => cos;
    "Tangent (radians)": tan / tanf (x) => tan;
    "Arcsine": asin / asinf (x) => asin;
    "Arccosine": acos / acosf (x) => acos;
    "Arctangent": atan / atanf (x) => atan;
    "Four-quadrant arctangent of `y / x`": atan2 / atan2f (y, x) => atan2;
    "Hyperbolic sine": sinh / sinhf (x) => sinh;
    "Hyperbolic cosine": cosh / coshf (x) => cosh;
    "Hyperbolic tangent": tanh / tanhf (x) => tanh;
    "Inverse hyperbolic sine": asinh / asinhf (x) => asinh;
    "Inverse hyperbolic cosine": acosh / acoshf (x) => acosh;
}

/// Inverse hyperbolic tangent (`f64`), evaluated on the magnitude to avoid
/// cancellation near `-1` in std's `ln_1p` formula, then given `x`'s sign.
#[inline]
#[must_use]
pub fn atanh(x: f64) -> f64 {
    f64::atanh(x.abs()).copysign(x)
}

/// Inverse hyperbolic tangent (`f32`); see [`atanh`].
#[inline]
#[must_use]
pub fn atanhf(x: f32) -> f32 {
    f32::atanh(x.abs()).copysign(x)
}

/// Positive difference: `x - y` when `x > y`, else `+0.0` (`f64`).
///
/// Upstream's `generic::fdim` body, so a NaN operand yields the NaN that
/// `x - y` produces, as upstream's does.
#[inline]
#[must_use]
pub fn fdim(x: f64, y: f64) -> f64 {
    if x <= y { 0.0 } else { x - y }
}

/// Positive difference: `x - y` when `x > y`, else `+0.0` (`f32`).
///
/// Upstream's `generic::fdim` body; see [`fdim`].
#[inline]
#[must_use]
pub fn fdimf(x: f32, y: f32) -> f32 {
    if x <= y { 0.0 } else { x - y }
}

/// `(sin(x), cos(x))` (`f64`); forwards to `f64::sin_cos`.
#[inline]
#[must_use]
pub fn sincos(x: f64) -> (f64, f64) {
    f64::sin_cos(x)
}

/// `(sin(x), cos(x))` (`f32`); forwards to `f32::sin_cos`.
#[inline]
#[must_use]
pub fn sincosf(x: f32) -> (f32, f32) {
    f32::sin_cos(x)
}
