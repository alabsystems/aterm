// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Limb left bitshift

use crate::{Limb, Word};
use core::ops::{Shl, ShlAssign};

impl Limb {
    /// Computes `self << rhs`.
    /// Panics if `rhs` overflows `Limb::BITS`.
    #[inline(always)]
    pub const fn shl(self, rhs: Self) -> Self {
        Limb(self.0 << rhs.0)
    }
}

impl Shl for Limb {
    type Output = Self;

    #[inline(always)]
    fn shl(self, rhs: Self) -> Self::Output {
        self.shl(rhs)
    }
}

impl Shl<usize> for Limb {
    type Output = Self;

    #[inline(always)]
    fn shl(self, rhs: usize) -> Self::Output {
        self.shl(Limb(rhs as Word))
    }
}

impl ShlAssign for Limb {
    #[inline(always)]
    fn shl_assign(&mut self, other: Self) {
        *self = self.shl(other);
    }
}

impl ShlAssign<usize> for Limb {
    #[inline(always)]
    fn shl_assign(&mut self, other: usize) {
        *self = self.shl(Limb(other as Word));
    }
}

#[cfg(test)]
#[path = "shl_tests.rs"]
mod tests;
