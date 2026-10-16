// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `libm`: a first-party facade for the crates.io package of the same name
//! (the directory is `crates/aterm-libm`; the manifest says why the names
//! differ). It is wired in through `[patch.crates-io]` and replaces the
//! vendored fork that lived at `vendor/libm` (19,867 lines and a build script,
//! forked only because upstream's `i64::abs` at `MIN` is an overflow Trust
//! refuses to compile).
//!
//! # Scope: the 68 functions the graph names
//!
//! Upstream is a pure-Rust port of musl's libm, written for `#![no_std]`
//! code. This crate is not a port: each function forwards to the `std`
//! method that computes the same thing, and only the functions a consumer in
//! aterm's graph names exist. Two consumers name 68 between them:
//!
//! * **num-traits 0.2.19**, `float.rs` `float_impl_libm!` and the `FloatCore`
//!   rounding methods: 33 functions per float width (66). They are compiled
//!   under `cfg(all(not(feature = "std"), feature = "libm"))`, and that is the
//!   live arm on every cell that has naga: naga and `half` take num-traits with
//!   `default-features = false`, and `half` turns on `libm`. So
//!   `num_traits::Float` for `f32`/`f64` IS this crate wherever wgpu ships.
//! * **naga 29**, `proc/constant_evaluator.rs`: `asinh`, `rint` and `rintf`
//!   (two new names) when it folds a constant expression.
//!
//! A new consumer that names another function fails to COMPILE, loudly; add
//! the forward here (or drop the patch row), never a silent fallback.
//!
//! # Fidelity
//!
//! * Bit-identical, exactly specified IEEE operations: `floor`, `ceil`,
//!   `round`, `trunc`, `fabs`, `copysign`, `sqrt`, `fma` (one rounding), `rint`
//!   (round half to even, upstream's answer in the default rounding mode),
//!   and `fdim`, whose body is upstream's `generic::fdim` reproduced.
//! * Implementation-defined, the transcendentals: std calls the platform's
//!   libm, which may differ from musl's by an ulp. On `wasm32-unknown-unknown`
//!   std's own math symbols come from compiler-builtins, which IS this
//!   upstream, so there the answers are upstream's. The exception is `atanh`
//!   / `atanhf`: std computes those itself on every target, as
//!   `0.5 * ((2x) / (1 - x)).ln_1p()`, not through the platform's libm. The
//!   facade evaluates their magnitude and restores the sign: evaluating that
//!   formula directly near `-1` loses precision when its ratio approaches `-1`.
//!
//! `tests/recorded.rs` holds both halves against answers recorded from the
//! retired fork before the switch; its ulp tolerance has been measured on
//! aarch64-apple-darwin only.
//!
//! # Not equivalent
//!
//! **Upstream is `#![no_std]`; this needs `std`.** Every aterm cell has it,
//! `wasm32-unknown-unknown` included. The features (`arch`,
//! `force-soft-floats`, `unstable*`) are accepted and ignored.
//!
//! The crate retires with naga: when no WGSL ships (the gpu-wgpu unit of
//! docs/AUDIT-third-party-2026-10-15.md), delete it and its patch row.

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
