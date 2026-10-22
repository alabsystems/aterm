// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from font-types 0.12.4 src/fixed.rs and src/point.rs and modified by
// the aterm project in 2026: the 16.16, 26.6 and 2.14 types and the point
// operations the autohinter and its outline loader use, nothing else.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! Fixed-point numbers: 16.16 ([`Fixed`]), 26.6 ([`F26Dot6`]) and 2.14
//! ([`F2Dot14`]), with FreeType's rounding, and a 2D [`Point`].

use core::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

macro_rules! fixed_impl {
    ($name:ident, $bits:literal, $fract_bits:literal, $ty:ty) => {
        #[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
        #[repr(transparent)]
        #[doc = concat!(stringify!($bits), "-bit signed fixed point number with ", stringify!($fract_bits), " bits of fraction." )]
        pub(crate) struct $name($ty);

        #[allow(dead_code)]
        impl $name {
            /// Representation of 0.0.
            pub const ZERO: Self = Self(0);

            /// Representation of 1.0.
            pub const ONE: Self = Self(1 << $fract_bits);

            /// Representation of -1.0.
            pub const NEG_ONE: Self = Self((!0 << $fract_bits) as $ty);

            const INT_MASK: $ty = !0 << $fract_bits;
            const ROUND: $ty = 1 << ($fract_bits - 1);
            const FRACT_BITS: usize = $fract_bits;

            /// Creates a new fixed point value from the underlying bit representation.
            #[inline(always)]
            pub const fn from_bits(bits: $ty) -> Self {
                Self(bits)
            }

            /// Returns the underlying bit representation of the value.
            #[inline(always)]
            pub const fn to_bits(self) -> $ty {
                self.0
            }

            /// Returns the nearest integer value.
            #[inline(always)]
            pub const fn round(self) -> Self {
                Self(self.0.wrapping_add(Self::ROUND) & Self::INT_MASK)
            }

            /// Returns the absolute value of the number.
            #[inline(always)]
            pub const fn abs(self) -> Self {
                Self(self.0.wrapping_abs())
            }

            /// Saturating subtraction.
            #[inline(always)]
            pub const fn saturating_sub(self, other: Self) -> Self {
                Self(self.0.saturating_sub(other.0))
            }
        }

        impl Add for $name {
            type Output = Self;
            #[inline(always)]
            fn add(self, other: Self) -> Self {
                Self(self.0.wrapping_add(other.0))
            }
        }

        impl AddAssign for $name {
            #[inline(always)]
            fn add_assign(&mut self, other: Self) {
                *self = *self + other;
            }
        }

        impl Sub for $name {
            type Output = Self;
            #[inline(always)]
            fn sub(self, other: Self) -> Self {
                Self(self.0.wrapping_sub(other.0))
            }
        }

        impl SubAssign for $name {
            #[inline(always)]
            fn sub_assign(&mut self, other: Self) {
                *self = *self - other;
            }
        }

        impl Neg for $name {
            type Output = Self;
            #[inline(always)]
            fn neg(self) -> Self {
                Self(self.0.wrapping_neg())
            }
        }
    };
}

fixed_impl!(F2Dot14, 16, 14, i16);
fixed_impl!(Fixed, 32, 16, i32);
fixed_impl!(F26Dot6, 32, 6, i32);

impl Mul for Fixed {
    type Output = Self;

    #[inline(always)]
    fn mul(self, other: Self) -> Self::Output {
        let ab = self.0 as i64 * other.0 as i64;
        Self(((ab + 0x8000 - i64::from(ab < 0)) >> 16) as i32)
    }
}

impl Div for Fixed {
    type Output = Self;

    #[inline]
    fn div(self, other: Self) -> Self {
        let sign = (self.0 < 0) ^ (other.0 < 0);
        let au = self.0.unsigned_abs() as u64;
        let bu = other.0.unsigned_abs() as u64;
        let q = (((au << 16) + (bu >> 1)).checked_div(bu)).map_or(0x7FFFFFFF_u32, |q| q as u32);
        Self(if sign {
            (q as i32).wrapping_neg()
        } else {
            q as i32
        })
    }
}

impl Fixed {
    /// Creates a 16.16 fixed point value from a 32 bit integer.
    #[inline(always)]
    pub const fn from_i32(i: i32) -> Self {
        Self(i << 16)
    }

    /// Converts a 16.16 fixed point value to a 32 bit integer, rounding off
    /// the fractional bits.
    #[inline(always)]
    pub const fn to_i32(self) -> i32 {
        self.0.wrapping_add(0x8000) >> 16
    }

    /// Converts a 16.16 to 2.14 fixed point value: add 2 and sign-extend
    /// shift right by 2, as the OpenType variations overview prescribes.
    #[inline(always)]
    pub const fn to_f2dot14(self) -> F2Dot14 {
        F2Dot14((self.0.wrapping_add(2) >> 2) as _)
    }

    /// Creates a fixed point value from an `f64`, rounded to the nearest
    /// representable value.
    #[inline(always)]
    pub fn from_f64(x: f64) -> Self {
        // When x is positive: 1.0 - 0.5 =  0.5
        // When x is negative: 0.0 - 0.5 = -0.5
        let frac = (x.is_sign_positive() as u8 as f64) - 0.5;
        Self((x * Self::ONE.0 as f64 + frac) as _)
    }
}

impl F26Dot6 {
    /// Creates a 26.6 fixed point value from a 32 bit integer.
    #[inline(always)]
    pub const fn from_i32(i: i32) -> Self {
        Self(i << 6)
    }

    /// Converts a 26.6 fixed point value to a 32 bit integer, rounding off
    /// the fractional bits.
    #[inline(always)]
    pub const fn to_i32(self) -> i32 {
        self.0.wrapping_add(32) >> 6
    }

    /// Converts a 26.6 fixed point value to a single precision floating
    /// point value.
    #[inline(always)]
    pub const fn to_f32(self) -> f32 {
        const SCALE_FACTOR: f32 = 1.0 / 64.0;
        self.0 as f32 * SCALE_FACTOR
    }
}

impl F2Dot14 {
    /// Converts a 2.14 to 16.16 fixed point value.
    #[inline(always)]
    pub const fn to_fixed(self) -> Fixed {
        Fixed(self.0 as i32 * 4)
    }
}

/// Two-dimensional point with a generic coordinate type.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub(crate) struct Point<T> {
    /// X coordinate.
    pub x: T,
    /// Y coordinate.
    pub y: T,
}

impl<T> Point<T> {
    /// Creates a new point with the given x and y coordinates.
    #[inline(always)]
    pub const fn new(x: T, y: T) -> Self {
        Self { x, y }
    }

    /// Creates a new point by applying the mapping function to each
    /// coordinate.
    #[inline(always)]
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Point<U> {
        Point {
            x: f(self.x),
            y: f(self.y),
        }
    }
}

impl<T: Add<Output = T>> Add for Point<T> {
    type Output = Self;

    #[inline(always)]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl<T: AddAssign> AddAssign for Point<T> {
    #[inline(always)]
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

impl<T: Sub<Output = T>> Sub for Point<T> {
    type Output = Self;

    #[inline(always)]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl<T: Mul<Output = T> + Copy> Mul<T> for Point<T> {
    type Output = Self;

    #[inline(always)]
    fn mul(self, rhs: T) -> Self::Output {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
        }
    }
}

#[cfg(test)]
mod tests {
    // font-types 0.12.4 src/fixed.rs, the tests of the operations this
    // subset keeps.
    use super::*;

    #[test]
    fn round_fixed() {
        assert_eq!(Fixed(0x0001_7FFE).round(), Fixed(0x0001_0000));
        assert_eq!(Fixed(0x0001_7FFF).round(), Fixed(0x0001_0000));
        assert_eq!(Fixed(0x0001_8000).round(), Fixed(0x0002_0000));
    }

    #[test]
    fn fixed_floats() {
        assert_eq!(Fixed(0x7fff_0000), Fixed::from_f64(32767.));
        assert_eq!(Fixed(0x7000_0001), Fixed::from_f64(28672.00001525879));
        assert_eq!(Fixed(0x0001_0000), Fixed::from_f64(1.0));
        assert_eq!(Fixed(0x0000_0000), Fixed::from_f64(0.0));
        assert_eq!(
            Fixed(i32::from_be_bytes([0xff; 4])),
            Fixed::from_f64(-0.000015259)
        );
        assert_eq!(Fixed(0x7fff_ffff), Fixed::from_f64(32768.0));
    }

    #[test]
    fn fixed_floats_rounding() {
        fn with_round_intrinsic(x: f64) -> Fixed {
            Fixed((x * 65536.0).round() as i32)
        }
        let inputs = [0.05, 0.6, 0.2, 0.4, 0.67755];
        for input in inputs {
            assert_eq!(Fixed::from_f64(input), with_round_intrinsic(input));
            assert_eq!(Fixed::from_f64(-input), with_round_intrinsic(-input));
        }
    }

    #[test]
    fn fixed_to_int() {
        assert_eq!(Fixed::from_f64(1.0).to_i32(), 1);
        assert_eq!(Fixed::from_f64(1.5).to_i32(), 2);
    }

    #[test]
    fn fixed_from_int() {
        assert_eq!(Fixed::from_i32(1000).to_bits(), 1000 << 16);
        assert_eq!(F26Dot6::from_i32(1000).to_bits(), 1000 << 6);
    }

    #[test]
    fn fixed_muldiv() {
        assert_eq!(
            Fixed::from_f64(0.5) * Fixed::from_f64(2.0),
            Fixed::from_f64(1.0)
        );
        assert_eq!(
            Fixed::from_f64(0.5) / Fixed::from_f64(2.0),
            Fixed::from_f64(0.25)
        );
    }

    #[test]
    fn fixed_mul_div_rounds_once_and_keeps_the_sign() {
        for (value, multiplier, divisor, expected) in [
            (1, 1, 1, 1),
            (1, 1, 2, 1),
            (-1, 1, 2, -1),
            (1, -1, 2, -1),
            (1, 1, -2, -1),
            (-1, -1, -2, -1),
            (2, 3, 4, 2),
            (6, -3, -2, 9),
        ] {
            assert_eq!(
                Fixed::from_bits(value)
                    .mul_div(Fixed::from_bits(multiplier), Fixed::from_bits(divisor))
                    .to_bits(),
                expected,
                "{value} * {multiplier} / {divisor}"
            );
        }
    }

    #[test]
    fn fixed_mul_div_handles_signed_extremes_and_zero_divisors() {
        for (value, multiplier, divisor, expected) in [
            (i32::MIN, 1, 1, i32::MIN),
            (i32::MIN, 1, -1, i32::MIN),
            (i32::MIN, -1, 2, 1 << 30),
            (i32::MAX, i32::MAX, i32::MAX, i32::MAX),
            (i32::MIN, i32::MIN, i32::MIN, i32::MIN),
            (i32::MAX, i32::MAX, 1, 1),
            (1, 1, 0, i32::MAX),
            (-1, 1, 0, -i32::MAX),
            (0, 0, 0, i32::MAX),
        ] {
            assert_eq!(
                Fixed::from_bits(value)
                    .mul_div(Fixed::from_bits(multiplier), Fixed::from_bits(divisor))
                    .to_bits(),
                expected,
                "{value} * {multiplier} / {divisor}"
            );
        }
    }

    #[test]
    fn fixed_div_neg_overflow() {
        let a = Fixed::from_f64(-92.5);
        let b = Fixed::from_f64(0.0028228759765625);
        let _ = a / b;
    }

    #[test]
    fn fixed_div_min_value() {
        let min = Fixed(i32::MIN);
        let _ = min / Fixed::ONE;
        let _ = min / Fixed(-Fixed::ONE.0);
    }

    #[test]
    fn fixed_abs_min_value() {
        assert_eq!(Fixed(i32::MIN).abs(), Fixed(i32::MIN));
    }

    #[test]
    fn fixed_neg_min_value() {
        assert_eq!(-Fixed(i32::MIN), Fixed(i32::MIN));
    }
}
