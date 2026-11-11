// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`Uint`] bitwise left shift operations.

use crate::{CtChoice, Limb, Uint, Word};
use core::ops::{Shl, ShlAssign};

impl<const LIMBS: usize> Uint<LIMBS> {
    /// Computes `self << shift` where `0 <= shift < Limb::BITS`,
    /// returning the result and the carry.
    #[inline(always)]
    pub(crate) const fn shl_limb(&self, n: usize) -> (Self, Limb) {
        let mut limbs = [Limb::ZERO; LIMBS];

        let nz = Limb(n as Word).ct_is_nonzero();
        let lshift = n as Word;
        let rshift = Limb::ct_select(Limb::ZERO, Limb((Limb::BITS - n) as Word), nz).0;
        let carry = Limb::ct_select(
            Limb::ZERO,
            Limb(self.limbs[LIMBS - 1].0.wrapping_shr(Word::BITS - n as u32)),
            nz,
        );

        let mut i = LIMBS - 1;
        while i > 0 {
            let mut limb = self.limbs[i].0 << lshift;
            let hi = self.limbs[i - 1].0 >> rshift;
            limb |= nz.if_true(hi);
            limbs[i] = Limb(limb);
            i -= 1
        }
        limbs[0] = Limb(self.limbs[0].0 << lshift);

        (Uint::<LIMBS>::new(limbs), carry)
    }

    /// Computes `self << shift`.
    ///
    /// NOTE: this operation is variable time with respect to `n` *ONLY*.
    ///
    /// When used with a fixed `n`, this function is constant-time with respect
    /// to `self`.
    #[inline(always)]
    pub const fn shl_vartime(&self, n: usize) -> Self {
        let mut limbs = [Limb::ZERO; LIMBS];

        if n >= Limb::BITS * LIMBS {
            return Self { limbs };
        }

        let shift_num = n / Limb::BITS;
        let rem = n % Limb::BITS;

        let mut i = LIMBS;
        while i > shift_num {
            i -= 1;
            limbs[i] = self.limbs[i - shift_num];
        }

        let (new_lower, _carry) = (Self { limbs }).shl_limb(rem);
        new_lower
    }

    /// Computes a left shift on a wide input as `(lo, hi)`.
    ///
    /// NOTE: this operation is variable time with respect to `n` *ONLY*.
    ///
    /// When used with a fixed `n`, this function is constant-time with respect
    /// to `self`.
    #[inline(always)]
    pub const fn shl_vartime_wide(lower_upper: (Self, Self), n: usize) -> (Self, Self) {
        let (lower, mut upper) = lower_upper;
        let new_lower = lower.shl_vartime(n);
        upper = upper.shl_vartime(n);
        if n >= Self::BITS {
            upper = upper.bitor(&lower.shl_vartime(n - Self::BITS));
        } else {
            upper = upper.bitor(&lower.shr_vartime(Self::BITS - n));
        }

        (new_lower, upper)
    }

    /// Computes `self << n`.
    /// Returns zero if `n >= Self::BITS`.
    pub const fn shl(&self, shift: usize) -> Self {
        let overflow = CtChoice::from_usize_lt(shift, Self::BITS).not();
        let shift = shift % Self::BITS;
        let mut result = *self;
        let mut i = 0;
        while i < Self::LOG2_BITS {
            let bit = CtChoice::from_lsb((shift as Word >> i) & 1);
            result = Uint::ct_select(&result, &result.shl_vartime(1 << i), bit);
            i += 1;
        }

        Uint::ct_select(&result, &Self::ZERO, overflow)
    }
}

impl<const LIMBS: usize> Shl<usize> for Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    /// NOTE: this operation is variable time with respect to `rhs` *ONLY*.
    ///
    /// When used with a fixed `rhs`, this function is constant-time with respect
    /// to `self`.
    fn shl(self, rhs: usize) -> Uint<LIMBS> {
        Uint::<LIMBS>::shl(&self, rhs)
    }
}

impl<const LIMBS: usize> Shl<usize> for &Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    /// NOTE: this operation is variable time with respect to `rhs` *ONLY*.
    ///
    /// When used with a fixed `rhs`, this function is constant-time with respect
    /// to `self`.
    fn shl(self, rhs: usize) -> Uint<LIMBS> {
        self.shl(rhs)
    }
}

impl<const LIMBS: usize> ShlAssign<usize> for Uint<LIMBS> {
    /// NOTE: this operation is variable time with respect to `rhs` *ONLY*.
    ///
    /// When used with a fixed `rhs`, this function is constant-time with respect
    /// to `self`.
    fn shl_assign(&mut self, rhs: usize) {
        *self = self.shl(rhs)
    }
}

#[cfg(test)]
#[path = "shl_tests.rs"]
mod tests;
