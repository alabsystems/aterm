// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `core_maths` — first-party replacement for the crates.io package of the same
//! name. The package is `core_maths`; the directory is `crates/aterm-core-maths`
//! (see the manifest for why the two names differ).
//!
//! Upstream is an extension trait that gives `#[no_std]` code the float methods
//! `std` puts on `f32`/`f64`, by forwarding each one to [`libm`]. This
//! replacement forwards them to `std` instead, and so brings NO dependency at
//! all.
//!
//! # What it retires
//!
//! `core_maths` is `libm`'s SOLE parent on two of aterm's five cells, and
//! `libm` is a VENDORED FORK — third-party source this repository must keep
//! reviewing forever. Dropping the edge takes both packages off:
//!
//! ```text
//! mac-arm   -2 packages, -21,088 lines, -1 build script
//!           (core_maths 1,221 + vendor/libm 19,867)
//! wasm-cpu  -2 packages, -21,088 lines, -1 build script
//! linux / win / wasm-gpu   -1 package, -1,221 lines
//!           (libm STAYS: naga and num-traits are still its parents there)
//! ```
//!
//! Every one of those edges is third-party — `cargo tree -e normal -i
//! core_maths` finds rustybuzz and ttf-parser and nothing of aterm's — so a
//! call-site census reports nothing to rewrite and the patch table is the only
//! lever that reaches it. Same shape as `cfg-if`, `profiling` and `log` before
//! it.
//!
//! # THE DIVERGENCE, stated first because it is the whole risk
//!
//! **Upstream is `#![no_std]`. This is not.** Upstream exists so that a crate
//! WITHOUT `std` can call `sin`; this shim answers by calling `std`. For a
//! genuine `no_std` consumer that is not an approximation, it is a build
//! failure — and that is acceptable here for a reason that is MEASURED rather
//! than assumed:
//!
//! Both consumers import the trait under a `cfg` that is off.
//!
//! ```text
//! rustybuzz-0.20.1/src/hb/face.rs:1                   #[cfg(not(feature = "std"))]
//! rustybuzz-0.20.1/src/hb/aat_layout_trak_table.rs:1  #[cfg(not(feature = "std"))]
//! rustybuzz-0.20.1/src/hb/ot_layout_gpos_table.rs:1   #[cfg(not(feature = "std"))]
//! ttf-parser-0.25.1/src/lib.rs:55                     #[cfg(not(feature = "std"))]
//! ```
//!
//! Those four lines are the ONLY occurrences of the string `core_maths` in
//! either crate's source, and `std` is enabled for both crates on all five
//! cells (`rustybuzz` defaults to `std`, whose definition is
//! `std = ["ttf-parser/std"]`; aterm takes rustybuzz with defaults). So in
//! aterm's graph the trait is never imported, and `.round()` in rustybuzz and
//! `.sin()`/`.cos()`/`.tan()`/`.abs()` in ttf-parser already resolve to `std`'s
//! inherent methods.
//!
//! **Not one method of this crate is called by anything aterm ships.** That is
//! the claim that makes the swap free of pixels, and `tests/consumers.rs` is
//! the armed tripwire on it: it re-derives the feature resolution for every
//! cell and fails, naming the cell, the day a `default-features = false` on
//! rustybuzz or ttf-parser turns `std` off and makes these bodies live.
//!
//! # If that tripwire ever fires
//!
//! The bodies below are still correct wherever `std` exists — which is every
//! target aterm builds, including `wasm32-unknown-unknown` — so the fix is
//! usually to leave `std` on. What genuinely does not work is building this
//! workspace for a target with no `std` at all; upstream would, and this will
//! not. Retire the `[patch.crates-io]` row for that build.
//!
//! # Fidelity to upstream
//!
//! Every method upstream forwards to a `libm` entry point is forwarded here to
//! the `std` inherent method of the same name. `std` and `libm` are two
//! implementations of the same functions, and for most of them IEEE 754 does
//! not specify a unique answer, so they may differ in the last bits:
//!
//! * **Bit-identical either way (12)** — `floor`, `ceil`, `round`, `trunc`,
//!   `fract`, `abs`, `copysign`, `sqrt`, `mul_add`, `signum`, `div_euclid`,
//!   `rem_euclid`: each is an exactly-specified IEEE operation, or pure IEEE
//!   arithmetic over such operations, with one correct result for every input.
//!   The last three are the arithmetic case: upstream writes
//!   `1.0.copysign(self)` behind a NaN check, `(self/rhs).trunc()` with a sign
//!   correction, and `self % rhs` with a fold into range — which is what `std`
//!   does, operation for operation, with no library call on either side.
//! * **Implementation-defined (25)** — `powf`, `exp`, `exp2`, `ln`, `log`,
//!   `log2`, `log10`, `cbrt`, `hypot`, `sin`, `cos`, `tan`, `sin_cos`, `asin`,
//!   `acos`, `atan`, `atan2`, `exp_m1`, `ln_1p`, `sinh`, `cosh`, `tanh`,
//!   `asinh`, `acosh`, `atanh`: correctly rounded by neither spec nor
//!   practice, so std and libm may differ by an ulp.
//! * **Bit-identical because upstream's body is reproduced here (1)** —
//!   [`powi`](CoreFloat::powi); see below.
//!
//! That is all 38 methods, and the count is the point: an earlier version of
//! this table listed 31 and read as if it were complete, which a judge caught.
//! The seven it omitted were `signum`, `div_euclid`, `rem_euclid`, `log`,
//! `asinh`, `acosh` and `atanh` — and four of those are in the second list, so
//! the omission hid risk rather than merely being untidy.
//!
//! One nuance the last four deserve, because the obvious reading is wrong.
//! `log` is `self.ln() / base.ln()` on both sides, and upstream's own doc says
//! of `asinh`/`acosh`/`atanh` that *"this method does not use an intrinsic in
//! `std`, so its code is copied"* — upstream COPIED std's formula. So the
//! formulas are identical and only the inner transcendentals differ (`ln`,
//! `ln_1p`, `hypot`, `sqrt` routed to `libm` there and to `std` here). They
//! belong in the implementation-defined list for that reason and no other:
//! not because the two implementations compute a different thing, but because
//! they compute the same expression over primitives that may each round
//! differently in the last bit.
//!
//! The implementation-defined list is unreachable in this graph (see above),
//! so the difference is currently unobservable — but it is real and it is why
//! this crate is a REPLACEMENT and not a re-export.
//!
//! [`powi`](CoreFloat::powi) is the one method NOT forwarded: `libm` has no
//! `powi`, so upstream writes the binary-exponentiation loop out by hand, and
//! `std`'s `powi` is an LLVM intrinsic that may multiply in a different order.
//! Upstream's loop is pure IEEE arithmetic with no library call, so it is
//! reproduced here verbatim and this method is bit-identical to the crate it
//! replaces.

#![deny(missing_docs)]
#![deny(rust_2018_idioms)]

/// Float methods that `core` does not have, for `#[no_std]` code.
///
/// Upstream backs each one with [`libm`]; this replacement backs them with
/// `std`. See the crate documentation for the fidelity table and for why the
/// difference is unobservable in aterm's graph.
pub trait CoreFloat: Sized + Copy {
    /// Largest integer less than or equal to `self`.
    fn floor(self) -> Self;
    /// Smallest integer greater than or equal to `self`.
    fn ceil(self) -> Self;
    /// Nearest integer to `self`; halfway cases round away from zero.
    fn round(self) -> Self;
    /// Integer part of `self`, truncated toward zero.
    fn trunc(self) -> Self;
    /// Fractional part of `self` — `self - self.trunc()`.
    fn fract(self) -> Self;
    /// Absolute value of `self`.
    fn abs(self) -> Self;
    /// `1.0` with `self`'s sign, or `NaN` if `self` is `NaN`.
    fn signum(self) -> Self;
    /// `self`'s magnitude with `sign`'s sign.
    fn copysign(self, sign: Self) -> Self;
    /// `(self * a) + b` with a single rounding (fused multiply-add).
    fn mul_add(self, a: Self, b: Self) -> Self;
    /// Euclidean quotient of `self` and `rhs`.
    fn div_euclid(self, rhs: Self) -> Self;
    /// Least non-negative remainder of `self (mod rhs)`.
    fn rem_euclid(self, rhs: Self) -> Self;
    /// `self` raised to an integer power.
    fn powi(self, n: i32) -> Self;
    /// `self` raised to a float power.
    fn powf(self, n: Self) -> Self;
    /// Square root of `self`.
    fn sqrt(self) -> Self;
    /// `e` raised to `self`.
    fn exp(self) -> Self;
    /// `2` raised to `self`.
    fn exp2(self) -> Self;
    /// Natural logarithm of `self`.
    fn ln(self) -> Self;
    /// Logarithm of `self` to an arbitrary `base`.
    fn log(self, base: Self) -> Self;
    /// Base-2 logarithm of `self`.
    fn log2(self) -> Self;
    /// Base-10 logarithm of `self`.
    fn log10(self) -> Self;
    /// Cube root of `self`.
    fn cbrt(self) -> Self;
    /// Length of the hypotenuse of a right triangle with legs `self`, `other`.
    fn hypot(self, other: Self) -> Self;
    /// Sine of `self`, in radians.
    fn sin(self) -> Self;
    /// Cosine of `self`, in radians.
    fn cos(self) -> Self;
    /// Tangent of `self`, in radians.
    fn tan(self) -> Self;
    /// Arcsine of `self`, in radians, in `[-pi/2, pi/2]`.
    fn asin(self) -> Self;
    /// Arccosine of `self`, in radians, in `[0, pi]`.
    fn acos(self) -> Self;
    /// Arctangent of `self`, in radians, in `[-pi/2, pi/2]`.
    fn atan(self) -> Self;
    /// Four-quadrant arctangent of `self` (y) and `other` (x), in radians.
    fn atan2(self, other: Self) -> Self;
    /// `(sin(self), cos(self))`.
    ///
    /// Upstream provides this as a defaulted method over [`sin`](Self::sin) and
    /// [`cos`](Self::cos) rather than as a `libm` entry point, and so does this
    /// replacement — the default body is reproduced verbatim.
    fn sin_cos(self) -> (Self, Self) {
        (self.sin(), self.cos())
    }
    /// `e^self - 1`, accurately even when `self` is near zero.
    fn exp_m1(self) -> Self;
    /// `ln(1 + self)`, accurately even when `self` is near zero.
    fn ln_1p(self) -> Self;
    /// Hyperbolic sine of `self`.
    fn sinh(self) -> Self;
    /// Hyperbolic cosine of `self`.
    fn cosh(self) -> Self;
    /// Hyperbolic tangent of `self`.
    fn tanh(self) -> Self;
    /// Inverse hyperbolic sine of `self`.
    fn asinh(self) -> Self;
    /// Inverse hyperbolic cosine of `self`.
    fn acosh(self) -> Self;
    /// Inverse hyperbolic tangent of `self`.
    fn atanh(self) -> Self;
}

/// Implement [`CoreFloat`] for one primitive float by forwarding to `std`.
///
/// Every body uses the QUALIFIED form `<$t>::name(self)`, never `self.name()`.
/// Both resolve to the inherent method — inherent items are found before trait
/// items — but a bare method call is one `use` away from resolving to
/// `CoreFloat` itself, which would compile cleanly and recurse forever. The
/// qualified form makes the target explicit, and `tests/forwarding.rs` calls
/// every method on both types so that a body which ever did recurse fails as a
/// stack overflow instead of shipping.
macro_rules! forward_to_std {
    ($t:ty) => {
        impl CoreFloat for $t {
            #[inline]
            fn floor(self) -> Self {
                <$t>::floor(self)
            }
            #[inline]
            fn ceil(self) -> Self {
                <$t>::ceil(self)
            }
            #[inline]
            fn round(self) -> Self {
                <$t>::round(self)
            }
            #[inline]
            fn trunc(self) -> Self {
                <$t>::trunc(self)
            }
            #[inline]
            fn fract(self) -> Self {
                <$t>::fract(self)
            }
            #[inline]
            fn abs(self) -> Self {
                <$t>::abs(self)
            }
            #[inline]
            fn signum(self) -> Self {
                <$t>::signum(self)
            }
            #[inline]
            fn copysign(self, sign: Self) -> Self {
                <$t>::copysign(self, sign)
            }
            #[inline]
            fn mul_add(self, a: Self, b: Self) -> Self {
                <$t>::mul_add(self, a, b)
            }
            #[inline]
            fn div_euclid(self, rhs: Self) -> Self {
                <$t>::div_euclid(self, rhs)
            }
            #[inline]
            fn rem_euclid(self, rhs: Self) -> Self {
                <$t>::rem_euclid(self, rhs)
            }
            /// NOT forwarded — upstream's own loop, reproduced verbatim.
            ///
            /// `libm` has no `powi`, so upstream implements it in pure IEEE
            /// arithmetic; `<$t>::powi` is `llvm.powi`, which is free to
            /// multiply in another order and round differently. Copying the
            /// loop is what makes this method bit-identical to the crate this
            /// one replaces, at the cost of the intrinsic's speed — a trade
            /// nothing in this graph can observe, since nothing calls it.
            #[inline]
            fn powi(self, exp: i32) -> Self {
                if exp == 0 {
                    return 1.0;
                }

                let mut base = if exp < 0 { self.recip() } else { self };
                let mut exp = exp.unsigned_abs();
                let mut acc = 1.0;

                while exp > 1 {
                    if (exp & 1) == 1 {
                        acc *= base;
                    }
                    exp /= 2;
                    base = base * base;
                }

                // `exp` is non-zero, so it is exactly 1 here: multiply the
                // final bit in without squaring the base again, which would be
                // needless and could overflow.
                acc * base
            }
            #[inline]
            fn powf(self, n: Self) -> Self {
                <$t>::powf(self, n)
            }
            #[inline]
            fn sqrt(self) -> Self {
                <$t>::sqrt(self)
            }
            #[inline]
            fn exp(self) -> Self {
                <$t>::exp(self)
            }
            #[inline]
            fn exp2(self) -> Self {
                <$t>::exp2(self)
            }
            #[inline]
            fn ln(self) -> Self {
                <$t>::ln(self)
            }
            #[inline]
            fn log(self, base: Self) -> Self {
                <$t>::log(self, base)
            }
            #[inline]
            fn log2(self) -> Self {
                <$t>::log2(self)
            }
            #[inline]
            fn log10(self) -> Self {
                <$t>::log10(self)
            }
            #[inline]
            fn cbrt(self) -> Self {
                <$t>::cbrt(self)
            }
            #[inline]
            fn hypot(self, other: Self) -> Self {
                <$t>::hypot(self, other)
            }
            #[inline]
            fn sin(self) -> Self {
                <$t>::sin(self)
            }
            #[inline]
            fn cos(self) -> Self {
                <$t>::cos(self)
            }
            #[inline]
            fn tan(self) -> Self {
                <$t>::tan(self)
            }
            #[inline]
            fn asin(self) -> Self {
                <$t>::asin(self)
            }
            #[inline]
            fn acos(self) -> Self {
                <$t>::acos(self)
            }
            #[inline]
            fn atan(self) -> Self {
                <$t>::atan(self)
            }
            #[inline]
            fn atan2(self, other: Self) -> Self {
                <$t>::atan2(self, other)
            }
            #[inline]
            fn exp_m1(self) -> Self {
                <$t>::exp_m1(self)
            }
            #[inline]
            fn ln_1p(self) -> Self {
                <$t>::ln_1p(self)
            }
            #[inline]
            fn sinh(self) -> Self {
                <$t>::sinh(self)
            }
            #[inline]
            fn cosh(self) -> Self {
                <$t>::cosh(self)
            }
            #[inline]
            fn tanh(self) -> Self {
                <$t>::tanh(self)
            }
            #[inline]
            fn asinh(self) -> Self {
                <$t>::asinh(self)
            }
            #[inline]
            fn acosh(self) -> Self {
                <$t>::acosh(self)
            }
            #[inline]
            fn atanh(self) -> Self {
                <$t>::atanh(self)
            }
        }
    };
}

forward_to_std!(f32);
forward_to_std!(f64);
