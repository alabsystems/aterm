// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Limb right bitshift

use crate::{Limb, Word};
use core::ops::{Shr, ShrAssign};

impl Limb {
    /// Computes `self >> rhs`.
    /// Panics if `rhs` overflows `Limb::BITS`.
    #[inline(always)]
    pub const fn shr(self, rhs: Self) -> Self {
        Limb(self.0 >> rhs.0)
    }
}

impl Shr for Limb {
    type Output = Self;

    #[inline(always)]
    fn shr(self, rhs: Self) -> Self::Output {
        self.shr(rhs)
    }
}

impl Shr<usize> for Limb {
    type Output = Self;

    #[inline(always)]
    fn shr(self, rhs: usize) -> Self::Output {
        self.shr(Limb(rhs as Word))
    }
}

impl ShrAssign for Limb {
    #[inline(always)]
    fn shr_assign(&mut self, other: Self) {
        *self = self.shr(other);
    }
}

impl ShrAssign<usize> for Limb {
    #[inline(always)]
    fn shr_assign(&mut self, other: usize) {
        *self = self.shr(Limb(other as Word));
    }
}

#[cfg(test)]
#[path = "shr_tests.rs"]
mod tests;
