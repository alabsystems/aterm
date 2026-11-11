// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`Uint`] bitwise not operations.

use super::Uint;
use crate::{Limb, Wrapping};
use core::ops::Not;

impl<const LIMBS: usize> Uint<LIMBS> {
    /// Computes bitwise `!a`.
    #[inline(always)]
    pub const fn not(&self) -> Self {
        let mut limbs = [Limb::ZERO; LIMBS];
        let mut i = 0;

        while i < LIMBS {
            limbs[i] = self.limbs[i].not();
            i += 1;
        }

        Self { limbs }
    }
}

impl<const LIMBS: usize> Not for Uint<LIMBS> {
    type Output = Self;

    #[allow(clippy::needless_borrow)]
    fn not(self) -> <Self as Not>::Output {
        (&self).not()
    }
}

impl<const LIMBS: usize> Not for Wrapping<Uint<LIMBS>> {
    type Output = Self;

    fn not(self) -> <Self as Not>::Output {
        Wrapping(self.0.not())
    }
}

#[cfg(test)]
#[path = "bit_not_tests.rs"]
mod tests;
