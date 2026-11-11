// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from font-types 0.12.4 src/fixed.rs and src/point.rs and modified by
// the aterm project in 2026: the 16.16, 26.6 and 2.14 types and the point
// operations the autohinter and its outline loader use, nothing else.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! Fixed-point numbers ([`Fixed`], [`F26Dot6`], [`F2Dot14`]) and 2D [`Point`].

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

    /// Converts a 16.16 fixed point value to a 32 bit integer, rounding off the fractional bits.
    #[inline(always)]
    pub const fn to_i32(self) -> i32 {
        self.0.wrapping_add(0x8000) >> 16
    }

    /// Converts a 16.16 to 2.14 fixed point value.
    #[inline(always)]
    pub const fn to_f2dot14(self) -> F2Dot14 {
        F2Dot14((self.0.wrapping_add(2) >> 2) as _)
    }

    /// Creates a fixed point value from an `f64`, rounded to the nearest representable value.
    #[inline(always)]
    pub fn from_f64(x: f64) -> Self {
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

    /// Converts a 26.6 fixed point value to a 32 bit integer, rounding off the fractional bits.
    #[inline(always)]
    pub const fn to_i32(self) -> i32 {
        self.0.wrapping_add(32) >> 6
    }

    /// Converts a 26.6 fixed point value to a single precision floating point value.
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

    /// Creates a new point by applying the mapping function to each coordinate.
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
#[path = "fixed_tests.rs"]
mod tests;
