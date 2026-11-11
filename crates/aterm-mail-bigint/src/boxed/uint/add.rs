// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`BoxedUint`] addition operations.

use crate::{BoxedUint, CheckedAdd, Limb, Zero};
use subtle::CtOption;

impl BoxedUint {
    /// Computes `a + b + carry`, returning the result along with the new carry.
    #[inline(always)]
    pub fn adc(&self, rhs: &Self, carry: Limb) -> (Self, Limb) {
        Self::chain(self, rhs, carry, |a, b, c| a.adc(b, c))
    }

    /// Perform wrapping addition, discarding overflow.
    pub fn wrapping_add(&self, rhs: &Self) -> Self {
        self.adc(rhs, Limb::ZERO).0
    }
}

impl CheckedAdd<&BoxedUint> for BoxedUint {
    type Output = Self;

    fn checked_add(&self, rhs: &Self) -> CtOption<Self> {
        let (result, carry) = self.adc(rhs, Limb::ZERO);
        CtOption::new(result, carry.is_zero())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "add_tests.rs"]
mod tests;
