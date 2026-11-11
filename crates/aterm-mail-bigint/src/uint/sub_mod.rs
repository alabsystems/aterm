// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`Uint`] subtraction modulus operations.

use crate::{Limb, SubMod, Uint};

impl<const LIMBS: usize> Uint<LIMBS> {
    /// Computes `self - rhs mod p`.
    ///
    /// Assumes `self - rhs` as unbounded signed integer is in `[-p, p)`.
    pub const fn sub_mod(&self, rhs: &Uint<LIMBS>, p: &Uint<LIMBS>) -> Uint<LIMBS> {
        let (out, borrow) = self.sbb(rhs, Limb::ZERO);

        // If underflow occurred on the final limb, borrow = 0xfff...fff, otherwise
        // borrow = 0x000...000. Thus, we use it as a mask to conditionally add the modulus.
        let mask = Uint::from_words([borrow.0; LIMBS]);

        out.wrapping_add(&p.bitand(&mask))
    }

    /// Returns `(self..., carry) - (rhs...) mod (p...)`, where `carry <= 1`.
    /// Assumes `-(p...) <= (self..., carry) - (rhs...) < (p...)`.
    #[inline(always)]
    pub(crate) const fn sub_mod_with_carry(&self, carry: Limb, rhs: &Self, p: &Self) -> Self {
        debug_assert!(carry.0 <= 1);

        let (out, borrow) = self.sbb(rhs, Limb::ZERO);

        // The new `borrow = Word::MAX` iff `carry == 0` and `borrow == Word::MAX`.
        let borrow = (!carry.0.wrapping_neg()) & borrow.0;

        // If underflow occurred on the final limb, borrow = 0xfff...fff, otherwise
        // borrow = 0x000...000. Thus, we use it as a mask to conditionally add the modulus.
        let mask = Uint::from_words([borrow; LIMBS]);

        out.wrapping_add(&p.bitand(&mask))
    }

    /// Computes `self - rhs mod p` for the special modulus
    /// `p = MAX+1-c` where `c` is small enough to fit in a single [`Limb`].
    ///
    /// Assumes `self - rhs` as unbounded signed integer is in `[-p, p)`.
    pub const fn sub_mod_special(&self, rhs: &Self, c: Limb) -> Self {
        let (out, borrow) = self.sbb(rhs, Limb::ZERO);

        // If underflow occurred, then we need to subtract `c` to account for
        // the underflow. This cannot underflow due to the assumption
        // `self - rhs >= -p`.
        let l = borrow.0 & c.0;
        out.wrapping_sub(&Uint::from_word(l))
    }
}

impl<const LIMBS: usize> SubMod for Uint<LIMBS> {
    type Output = Self;

    fn sub_mod(&self, rhs: &Self, p: &Self) -> Self {
        debug_assert!(self < p);
        debug_assert!(rhs < p);
        self.sub_mod(rhs, p)
    }
}

#[cfg(all(test, feature = "rand"))]
#[path = "sub_mod_tests.rs"]
mod tests;
