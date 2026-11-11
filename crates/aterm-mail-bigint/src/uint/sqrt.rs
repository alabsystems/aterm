// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`Uint`] square root operations.

use super::Uint;
use crate::{Limb, Word};
use subtle::{ConstantTimeEq, CtOption};

impl<const LIMBS: usize> Uint<LIMBS> {
    /// See [`Self::sqrt_vartime`].
    #[deprecated(
        since = "0.5.3",
        note = "This functionality will be moved to `sqrt_vartime` in a future release."
    )]
    pub const fn sqrt(&self) -> Self {
        self.sqrt_vartime()
    }

    /// Computes √(`self`)
    /// Uses Brent & Zimmermann, Modern Computer Arithmetic, v0.5.9, Algorithm 1.13
    ///
    /// Callers can check if `self` is a square by squaring the result
    pub const fn sqrt_vartime(&self) -> Self {
        let max_bits = (self.bits_vartime() + 1) >> 1;
        let cap = Self::ONE.shl_vartime(max_bits);
        let mut guess = cap; // ≥ √(`self`)
        let mut xn = {
            let q = self.wrapping_div(&guess);
            let t = guess.wrapping_add(&q);
            t.shr_vartime(1)
        };

        // If guess increased, the initial guess was low.
        // Repeat until reverse course.
        while Uint::ct_lt(&guess, &xn).is_true_vartime() {
            // Sometimes an increase is too far, especially with large
            // powers, and then takes a long time to walk back.  The upper
            // bound is based on bit size, so saturate on that.
            let le = Limb::ct_le(Limb(xn.bits_vartime() as Word), Limb(max_bits as Word));
            guess = Self::ct_select(&cap, &xn, le);
            xn = {
                let q = self.wrapping_div(&guess);
                let t = guess.wrapping_add(&q);
                t.shr_vartime(1)
            };
        }

        // Repeat while guess decreases.
        while Uint::ct_gt(&guess, &xn).is_true_vartime() && xn.ct_is_nonzero().is_true_vartime() {
            guess = xn;
            xn = {
                let q = self.wrapping_div(&guess);
                let t = guess.wrapping_add(&q);
                t.shr_vartime(1)
            };
        }

        Self::ct_select(&Self::ZERO, &guess, self.ct_is_nonzero())
    }

    /// See [`Self::wrapping_sqrt_vartime`].
    #[deprecated(
        since = "0.5.3",
        note = "This functionality will be moved to `wrapping_sqrt_vartime` in a future release."
    )]
    pub const fn wrapping_sqrt(&self) -> Self {
        self.wrapping_sqrt_vartime()
    }

    /// Wrapped sqrt is just normal √(`self`)
    /// There’s no way wrapping could ever happen.
    /// This function exists, so that all operations are accounted for in the wrapping operations.
    pub const fn wrapping_sqrt_vartime(&self) -> Self {
        self.sqrt_vartime()
    }

    /// See [`Self::checked_sqrt_vartime`].
    #[deprecated(
        since = "0.5.3",
        note = "This functionality will be moved to `checked_sqrt_vartime` in a future release."
    )]
    pub fn checked_sqrt(&self) -> CtOption<Self> {
        self.checked_sqrt_vartime()
    }

    /// Perform checked sqrt, returning a [`CtOption`] which `is_some`
    /// only if the √(`self`)² == self
    pub fn checked_sqrt_vartime(&self) -> CtOption<Self> {
        let r = self.sqrt_vartime();
        let s = r.wrapping_mul(&r);
        CtOption::new(r, ConstantTimeEq::ct_eq(self, &s))
    }
}

#[cfg(test)]
#[path = "sqrt_tests.rs"]
mod tests;
