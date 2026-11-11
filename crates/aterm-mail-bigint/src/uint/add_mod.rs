// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`Uint`] addition modulus operations.

use crate::{AddMod, Limb, Uint};

impl<const LIMBS: usize> Uint<LIMBS> {
    /// Computes `self + rhs mod p`.
    ///
    /// Assumes `self + rhs` as unbounded integer is `< 2p`.
    pub const fn add_mod(&self, rhs: &Uint<LIMBS>, p: &Uint<LIMBS>) -> Uint<LIMBS> {
        let (w, carry) = self.adc(rhs, Limb::ZERO);

        // Attempt to subtract the modulus, to ensure the result is in the field.
        let (w, borrow) = w.sbb(p, Limb::ZERO);
        let (_, borrow) = carry.sbb(Limb::ZERO, borrow);

        // If underflow occurred on the final limb, borrow = 0xfff...fff, otherwise
        // borrow = 0x000...000. Thus, we use it as a mask to conditionally add the
        // modulus.
        let mask = Uint::from_words([borrow.0; LIMBS]);

        w.wrapping_add(&p.bitand(&mask))
    }

    /// Computes `self + rhs mod p` for the special modulus
    /// `p = MAX+1-c` where `c` is small enough to fit in a single [`Limb`].
    ///
    /// Assumes `self + rhs` as unbounded integer is `< 2p`.
    pub const fn add_mod_special(&self, rhs: &Self, c: Limb) -> Self {
        // `Uint::adc` also works with a carry greater than 1.
        let (out, carry) = self.adc(rhs, c);

        // If overflow occurred, then above addition of `c` already accounts
        // for the overflow. Otherwise, we need to subtract `c` again, which
        // in that case cannot underflow.
        let l = carry.0.wrapping_sub(1) & c.0;
        out.wrapping_sub(&Uint::from_word(l))
    }
}

impl<const LIMBS: usize> AddMod for Uint<LIMBS> {
    type Output = Self;

    fn add_mod(&self, rhs: &Self, p: &Self) -> Self {
        debug_assert!(self < p);
        debug_assert!(rhs < p);
        self.add_mod(rhs, p)
    }
}

#[cfg(all(test, feature = "rand"))]
#[path = "add_mod_tests.rs"]
mod tests;
