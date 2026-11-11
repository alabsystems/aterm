// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Limb comparisons

use super::HI_BIT;
use crate::{CtChoice, Limb};
use core::cmp::Ordering;
use subtle::{Choice, ConstantTimeEq, ConstantTimeGreater, ConstantTimeLess};

impl Limb {
    /// Is this limb an odd number?
    #[inline]
    pub fn is_odd(&self) -> Choice {
        Choice::from(self.0 as u8 & 1)
    }

    /// Perform a comparison of the inner value in variable-time.
    ///
    /// Note that the [`PartialOrd`] and [`Ord`] impls wrap constant-time
    /// comparisons using the `subtle` crate.
    pub fn cmp_vartime(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }

    /// Performs an equality check in variable-time.
    pub const fn eq_vartime(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    /// Return `b` if `c` is truthy, otherwise return `a`.
    #[inline]
    pub(crate) const fn ct_select(a: Self, b: Self, c: CtChoice) -> Self {
        Self(c.select(a.0, b.0))
    }

    /// Returns the truthy value if `self != 0` and the falsy value otherwise.
    #[inline]
    pub(crate) const fn ct_is_nonzero(&self) -> CtChoice {
        let inner = self.0;
        CtChoice::from_lsb((inner | inner.wrapping_neg()) >> HI_BIT)
    }

    /// Returns the truthy value if `lhs == rhs` and the falsy value otherwise.
    #[inline]
    pub(crate) const fn ct_eq(lhs: Self, rhs: Self) -> CtChoice {
        let x = lhs.0;
        let y = rhs.0;

        // x ^ y == 0 if and only if x == y
        Self(x ^ y).ct_is_nonzero().not()
    }

    /// Returns the truthy value if `lhs < rhs` and the falsy value otherwise.
    #[inline]
    pub(crate) const fn ct_lt(lhs: Self, rhs: Self) -> CtChoice {
        let x = lhs.0;
        let y = rhs.0;
        let bit = (((!x) & y) | (((!x) | y) & (x.wrapping_sub(y)))) >> (Limb::BITS - 1);
        CtChoice::from_lsb(bit)
    }

    /// Returns the truthy value if `lhs <= rhs` and the falsy value otherwise.
    #[inline]
    pub(crate) const fn ct_le(lhs: Self, rhs: Self) -> CtChoice {
        let x = lhs.0;
        let y = rhs.0;
        let bit = (((!x) | y) & ((x ^ y) | !(y.wrapping_sub(x)))) >> (Limb::BITS - 1);
        CtChoice::from_lsb(bit)
    }
}

impl ConstantTimeEq for Limb {
    #[inline]
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.ct_eq(&other.0)
    }
}

impl ConstantTimeGreater for Limb {
    #[inline]
    fn ct_gt(&self, other: &Self) -> Choice {
        self.0.ct_gt(&other.0)
    }
}

impl ConstantTimeLess for Limb {
    #[inline]
    fn ct_lt(&self, other: &Self) -> Choice {
        self.0.ct_lt(&other.0)
    }
}

impl Eq for Limb {}

impl Ord for Limb {
    fn cmp(&self, other: &Self) -> Ordering {
        let mut n = 0i8;
        n -= self.ct_lt(other).unwrap_u8() as i8;
        n += self.ct_gt(other).unwrap_u8() as i8;

        match n {
            -1 => Ordering::Less,
            1 => Ordering::Greater,
            _ => {
                debug_assert_eq!(n, 0);
                debug_assert!(bool::from(self.ct_eq(other)));
                Ordering::Equal
            }
        }
    }
}

impl PartialOrd for Limb {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Limb {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).into()
    }
}

#[cfg(test)]
#[path = "cmp_tests.rs"]
mod tests;
