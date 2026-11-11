// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`Uint`] division operations.

use super::div_limb::{Reciprocal, div_rem_limb_with_reciprocal};
use crate::{CtChoice, Limb, NonZero, Uint, Word, Wrapping};
use core::ops::{Div, DivAssign, Rem, RemAssign};
use subtle::CtOption;

impl<const LIMBS: usize> Uint<LIMBS> {
    /// Computes `self` / `rhs` using a pre-made reciprocal,
    /// returns the quotient (q) and remainder (r).
    #[inline(always)]
    pub const fn ct_div_rem_limb_with_reciprocal(&self, reciprocal: &Reciprocal) -> (Self, Limb) {
        div_rem_limb_with_reciprocal(self, reciprocal)
    }

    /// Computes `self` / `rhs` using a pre-made reciprocal,
    /// returns the quotient (q) and remainder (r).
    #[inline(always)]
    pub fn div_rem_limb_with_reciprocal(
        &self,
        reciprocal: &CtOption<Reciprocal>,
    ) -> CtOption<(Self, Limb)> {
        reciprocal.map(|r| div_rem_limb_with_reciprocal(self, &r))
    }

    /// Computes `self` / `rhs`, returns the quotient (q) and remainder (r).
    /// Returns the truthy value as the third element of the tuple if `rhs != 0`,
    /// and the falsy value otherwise.
    #[inline(always)]
    pub(crate) const fn ct_div_rem_limb(&self, rhs: Limb) -> (Self, Limb, CtChoice) {
        let (reciprocal, is_some) = Reciprocal::ct_new(rhs);
        let (quo, rem) = div_rem_limb_with_reciprocal(self, &reciprocal);
        (quo, rem, is_some)
    }

    /// Computes `self` / `rhs`, returns the quotient (q) and remainder (r).
    #[inline(always)]
    pub fn div_rem_limb(&self, rhs: NonZero<Limb>) -> (Self, Limb) {
        // Guaranteed to succeed since `rhs` is nonzero.
        let (quo, rem, _is_some) = self.ct_div_rem_limb(*rhs);
        (quo, rem)
    }

    /// Computes `self` / `rhs`, returns the quotient (q), remainder (r)
    /// and the truthy value for is_some or the falsy value for is_none.
    ///
    /// NOTE: Use only if you need to access const fn. Otherwise use [`Self::div_rem`] because
    /// the value for is_some needs to be checked before using `q` and `r`.
    ///
    /// This is variable only with respect to `rhs`.
    ///
    /// When used with a fixed `rhs`, this function is constant-time with respect
    /// to `self`.
    pub(crate) const fn ct_div_rem(&self, rhs: &Self) -> (Self, Self, CtChoice) {
        let mb = rhs.bits_vartime();
        let mut bd = Self::BITS - mb;
        let mut rem = *self;
        let mut quo = Self::ZERO;
        let mut c = rhs.shl_vartime(bd);

        loop {
            let (mut r, borrow) = rem.sbb(&c, Limb::ZERO);
            rem = Self::ct_select(&r, &rem, CtChoice::from_mask(borrow.0));
            r = quo.bitor(&Self::ONE);
            quo = Self::ct_select(&r, &quo, CtChoice::from_mask(borrow.0));
            if bd == 0 {
                break;
            }
            bd -= 1;
            c = c.shr_vartime(1);
            quo = quo.shl_vartime(1);
        }

        let is_some = Limb(mb as Word).ct_is_nonzero();
        quo = Self::ct_select(&Self::ZERO, &quo, is_some);
        (quo, rem, is_some)
    }

    /// Computes `self` % `rhs`, returns the remainder and
    /// and the truthy value for is_some or the falsy value for is_none.
    ///
    /// NOTE: Use only if you need to access const fn. Otherwise use [`Self::rem`].
    /// This is variable only with respect to `rhs`.
    ///
    /// When used with a fixed `rhs`, this function is constant-time with respect
    /// to `self`.
    pub const fn const_rem(&self, rhs: &Self) -> (Self, CtChoice) {
        let mb = rhs.bits_vartime();
        let mut bd = Self::BITS - mb;
        let mut rem = *self;
        let mut c = rhs.shl_vartime(bd);

        loop {
            let (r, borrow) = rem.sbb(&c, Limb::ZERO);
            rem = Self::ct_select(&r, &rem, CtChoice::from_mask(borrow.0));
            if bd == 0 {
                break;
            }
            bd -= 1;
            c = c.shr_vartime(1);
        }

        let is_some = Limb(mb as Word).ct_is_nonzero();
        (rem, is_some)
    }

    /// Computes `self` % `rhs`, returns the remainder and
    /// and the truthy value for is_some or the falsy value for is_none.
    ///
    /// This is variable only with respect to `rhs`.
    ///
    /// When used with a fixed `rhs`, this function is constant-time with respect
    /// to `self`.
    pub const fn const_rem_wide(lower_upper: (Self, Self), rhs: &Self) -> (Self, CtChoice) {
        let mb = rhs.bits_vartime();

        // The number of bits to consider is two sets of limbs * BITS - mb (modulus bitcount)
        let mut bd = (2 * Self::BITS) - mb;

        // The wide integer to reduce, split into two halves
        let (mut lower, mut upper) = lower_upper;

        // Factor of the modulus, split into two halves
        let mut c = Self::shl_vartime_wide((*rhs, Uint::ZERO), bd);

        loop {
            let (lower_sub, borrow) = lower.sbb(&c.0, Limb::ZERO);
            let (upper_sub, borrow) = upper.sbb(&c.1, borrow);

            lower = Self::ct_select(&lower_sub, &lower, CtChoice::from_mask(borrow.0));
            upper = Self::ct_select(&upper_sub, &upper, CtChoice::from_mask(borrow.0));
            if bd == 0 {
                break;
            }
            bd -= 1;
            c = Self::shr_vartime_wide(c, 1);
        }

        let is_some = Limb(mb as Word).ct_is_nonzero();
        (lower, is_some)
    }

    /// Computes `self` % 2^k. Faster than reduce since its a power of 2.
    /// Limited to 2^16-1 since Uint doesn't support higher.
    pub const fn rem2k(&self, k: usize) -> Self {
        let highest = (LIMBS - 1) as u32;
        let index = k as u32 / (Limb::BITS as u32);
        let le = Limb::ct_le(Limb::from_u32(index), Limb::from_u32(highest));
        let word = Limb::ct_select(Limb::from_u32(highest), Limb::from_u32(index), le).0 as usize;

        let base = k % Limb::BITS;
        let mask = (1 << base) - 1;
        let mut out = *self;

        let outmask = Limb(out.limbs[word].0 & mask);

        out.limbs[word] = Limb::ct_select(out.limbs[word], outmask, le);

        let mut i = word + 1;
        while i < LIMBS {
            out.limbs[i] = Limb::ZERO;
            i += 1;
        }

        out
    }

    /// Computes self / rhs, returns the quotient, remainder.
    pub fn div_rem(&self, rhs: &NonZero<Self>) -> (Self, Self) {
        // Since `rhs` is nonzero, this should always hold.
        let (q, r, _c) = self.ct_div_rem(rhs);
        (q, r)
    }

    /// Computes self % rhs, returns the remainder.
    pub fn rem(&self, rhs: &NonZero<Self>) -> Self {
        // Since `rhs` is nonzero, this should always hold.
        let (r, _c) = self.const_rem(rhs);
        r
    }

    /// Wrapped division is just normal division i.e. `self` / `rhs`
    /// There’s no way wrapping could ever happen.
    /// This function exists, so that all operations are accounted for in the wrapping operations.
    ///
    /// Panics if `rhs == 0`.
    pub const fn wrapping_div(&self, rhs: &Self) -> Self {
        let (q, _, c) = self.ct_div_rem(rhs);
        assert!(c.is_true_vartime(), "divide by zero");
        q
    }

    /// Perform checked division, returning a [`CtOption`] which `is_some`
    /// only if the rhs != 0
    pub fn checked_div(&self, rhs: &Self) -> CtOption<Self> {
        NonZero::new(*rhs).map(|rhs| {
            let (q, _r) = self.div_rem(&rhs);
            q
        })
    }

    /// Wrapped (modular) remainder calculation is just `self` % `rhs`.
    /// There’s no way wrapping could ever happen.
    /// This function exists, so that all operations are accounted for in the wrapping operations.
    ///
    /// Panics if `rhs == 0`.
    pub const fn wrapping_rem(&self, rhs: &Self) -> Self {
        let (r, c) = self.const_rem(rhs);
        assert!(c.is_true_vartime(), "modulo zero");
        r
    }

    /// Perform checked reduction, returning a [`CtOption`] which `is_some`
    /// only if the rhs != 0
    pub fn checked_rem(&self, rhs: &Self) -> CtOption<Self> {
        NonZero::new(*rhs).map(|rhs| self.rem(&rhs))
    }
}

//
// Division by a single limb
//

impl<const LIMBS: usize> Div<&NonZero<Limb>> for &Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn div(self, rhs: &NonZero<Limb>) -> Self::Output {
        *self / *rhs
    }
}

impl<const LIMBS: usize> Div<&NonZero<Limb>> for Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn div(self, rhs: &NonZero<Limb>) -> Self::Output {
        self / *rhs
    }
}

impl<const LIMBS: usize> Div<NonZero<Limb>> for &Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn div(self, rhs: NonZero<Limb>) -> Self::Output {
        *self / rhs
    }
}

impl<const LIMBS: usize> Div<NonZero<Limb>> for Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn div(self, rhs: NonZero<Limb>) -> Self::Output {
        let (q, _, _) = self.ct_div_rem_limb(*rhs);
        q
    }
}

impl<const LIMBS: usize> DivAssign<&NonZero<Limb>> for Uint<LIMBS> {
    fn div_assign(&mut self, rhs: &NonZero<Limb>) {
        *self /= *rhs;
    }
}

impl<const LIMBS: usize> DivAssign<NonZero<Limb>> for Uint<LIMBS> {
    fn div_assign(&mut self, rhs: NonZero<Limb>) {
        *self = *self / rhs;
    }
}

impl<const LIMBS: usize> Div<NonZero<Limb>> for Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn div(self, rhs: NonZero<Limb>) -> Self::Output {
        Wrapping(self.0 / rhs)
    }
}

impl<const LIMBS: usize> Div<NonZero<Limb>> for &Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn div(self, rhs: NonZero<Limb>) -> Self::Output {
        *self / rhs
    }
}

impl<const LIMBS: usize> Div<&NonZero<Limb>> for &Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn div(self, rhs: &NonZero<Limb>) -> Self::Output {
        *self / *rhs
    }
}

impl<const LIMBS: usize> Div<&NonZero<Limb>> for Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn div(self, rhs: &NonZero<Limb>) -> Self::Output {
        self / *rhs
    }
}

impl<const LIMBS: usize> DivAssign<&NonZero<Limb>> for Wrapping<Uint<LIMBS>> {
    fn div_assign(&mut self, rhs: &NonZero<Limb>) {
        *self = Wrapping(self.0 / rhs)
    }
}

impl<const LIMBS: usize> DivAssign<NonZero<Limb>> for Wrapping<Uint<LIMBS>> {
    fn div_assign(&mut self, rhs: NonZero<Limb>) {
        *self /= &rhs;
    }
}

impl<const LIMBS: usize> Rem<&NonZero<Limb>> for &Uint<LIMBS> {
    type Output = Limb;

    fn rem(self, rhs: &NonZero<Limb>) -> Self::Output {
        *self % *rhs
    }
}

impl<const LIMBS: usize> Rem<&NonZero<Limb>> for Uint<LIMBS> {
    type Output = Limb;

    fn rem(self, rhs: &NonZero<Limb>) -> Self::Output {
        self % *rhs
    }
}

impl<const LIMBS: usize> Rem<NonZero<Limb>> for &Uint<LIMBS> {
    type Output = Limb;

    fn rem(self, rhs: NonZero<Limb>) -> Self::Output {
        *self % rhs
    }
}

impl<const LIMBS: usize> Rem<NonZero<Limb>> for Uint<LIMBS> {
    type Output = Limb;

    fn rem(self, rhs: NonZero<Limb>) -> Self::Output {
        let (_, r, _) = self.ct_div_rem_limb(*rhs);
        r
    }
}

impl<const LIMBS: usize> RemAssign<&NonZero<Limb>> for Uint<LIMBS> {
    fn rem_assign(&mut self, rhs: &NonZero<Limb>) {
        *self = (*self % rhs).into();
    }
}

impl<const LIMBS: usize> RemAssign<NonZero<Limb>> for Uint<LIMBS> {
    fn rem_assign(&mut self, rhs: NonZero<Limb>) {
        *self %= &rhs;
    }
}

impl<const LIMBS: usize> Rem<NonZero<Limb>> for Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Limb>;

    fn rem(self, rhs: NonZero<Limb>) -> Self::Output {
        Wrapping(self.0 % rhs)
    }
}

impl<const LIMBS: usize> Rem<NonZero<Limb>> for &Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Limb>;

    fn rem(self, rhs: NonZero<Limb>) -> Self::Output {
        *self % rhs
    }
}

impl<const LIMBS: usize> Rem<&NonZero<Limb>> for &Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Limb>;

    fn rem(self, rhs: &NonZero<Limb>) -> Self::Output {
        *self % *rhs
    }
}

impl<const LIMBS: usize> Rem<&NonZero<Limb>> for Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Limb>;

    fn rem(self, rhs: &NonZero<Limb>) -> Self::Output {
        self % *rhs
    }
}

impl<const LIMBS: usize> RemAssign<NonZero<Limb>> for Wrapping<Uint<LIMBS>> {
    fn rem_assign(&mut self, rhs: NonZero<Limb>) {
        *self %= &rhs;
    }
}

impl<const LIMBS: usize> RemAssign<&NonZero<Limb>> for Wrapping<Uint<LIMBS>> {
    fn rem_assign(&mut self, rhs: &NonZero<Limb>) {
        *self = Wrapping((self.0 % rhs).into())
    }
}

//
// Division by an Uint
//

impl<const LIMBS: usize> Div<&NonZero<Uint<LIMBS>>> for &Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn div(self, rhs: &NonZero<Uint<LIMBS>>) -> Self::Output {
        *self / *rhs
    }
}

impl<const LIMBS: usize> Div<&NonZero<Uint<LIMBS>>> for Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn div(self, rhs: &NonZero<Uint<LIMBS>>) -> Self::Output {
        self / *rhs
    }
}

impl<const LIMBS: usize> Div<NonZero<Uint<LIMBS>>> for &Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn div(self, rhs: NonZero<Uint<LIMBS>>) -> Self::Output {
        *self / rhs
    }
}

impl<const LIMBS: usize> Div<NonZero<Uint<LIMBS>>> for Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn div(self, rhs: NonZero<Uint<LIMBS>>) -> Self::Output {
        let (q, _) = self.div_rem(&rhs);
        q
    }
}

impl<const LIMBS: usize> DivAssign<&NonZero<Uint<LIMBS>>> for Uint<LIMBS> {
    fn div_assign(&mut self, rhs: &NonZero<Uint<LIMBS>>) {
        *self /= *rhs
    }
}

impl<const LIMBS: usize> DivAssign<NonZero<Uint<LIMBS>>> for Uint<LIMBS> {
    fn div_assign(&mut self, rhs: NonZero<Uint<LIMBS>>) {
        *self = *self / rhs;
    }
}

impl<const LIMBS: usize> Div<NonZero<Uint<LIMBS>>> for Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn div(self, rhs: NonZero<Uint<LIMBS>>) -> Self::Output {
        Wrapping(self.0 / rhs)
    }
}

impl<const LIMBS: usize> Div<NonZero<Uint<LIMBS>>> for &Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn div(self, rhs: NonZero<Uint<LIMBS>>) -> Self::Output {
        *self / rhs
    }
}

impl<const LIMBS: usize> Div<&NonZero<Uint<LIMBS>>> for &Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn div(self, rhs: &NonZero<Uint<LIMBS>>) -> Self::Output {
        *self / *rhs
    }
}

impl<const LIMBS: usize> Div<&NonZero<Uint<LIMBS>>> for Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn div(self, rhs: &NonZero<Uint<LIMBS>>) -> Self::Output {
        self / *rhs
    }
}

impl<const LIMBS: usize> DivAssign<&NonZero<Uint<LIMBS>>> for Wrapping<Uint<LIMBS>> {
    fn div_assign(&mut self, rhs: &NonZero<Uint<LIMBS>>) {
        *self = Wrapping(self.0 / rhs);
    }
}

impl<const LIMBS: usize> DivAssign<NonZero<Uint<LIMBS>>> for Wrapping<Uint<LIMBS>> {
    fn div_assign(&mut self, rhs: NonZero<Uint<LIMBS>>) {
        *self /= &rhs;
    }
}

impl<const LIMBS: usize> Rem<&NonZero<Uint<LIMBS>>> for &Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn rem(self, rhs: &NonZero<Uint<LIMBS>>) -> Self::Output {
        *self % *rhs
    }
}

impl<const LIMBS: usize> Rem<&NonZero<Uint<LIMBS>>> for Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn rem(self, rhs: &NonZero<Uint<LIMBS>>) -> Self::Output {
        self % *rhs
    }
}

impl<const LIMBS: usize> Rem<NonZero<Uint<LIMBS>>> for &Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn rem(self, rhs: NonZero<Uint<LIMBS>>) -> Self::Output {
        *self % rhs
    }
}

impl<const LIMBS: usize> Rem<NonZero<Uint<LIMBS>>> for Uint<LIMBS> {
    type Output = Uint<LIMBS>;

    fn rem(self, rhs: NonZero<Uint<LIMBS>>) -> Self::Output {
        Self::rem(&self, &rhs)
    }
}

impl<const LIMBS: usize> RemAssign<&NonZero<Uint<LIMBS>>> for Uint<LIMBS> {
    fn rem_assign(&mut self, rhs: &NonZero<Uint<LIMBS>>) {
        *self %= *rhs
    }
}

impl<const LIMBS: usize> RemAssign<NonZero<Uint<LIMBS>>> for Uint<LIMBS> {
    fn rem_assign(&mut self, rhs: NonZero<Uint<LIMBS>>) {
        *self = *self % rhs;
    }
}

impl<const LIMBS: usize> Rem<NonZero<Uint<LIMBS>>> for Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn rem(self, rhs: NonZero<Uint<LIMBS>>) -> Self::Output {
        Wrapping(self.0 % rhs)
    }
}

impl<const LIMBS: usize> Rem<NonZero<Uint<LIMBS>>> for &Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn rem(self, rhs: NonZero<Uint<LIMBS>>) -> Self::Output {
        *self % rhs
    }
}

impl<const LIMBS: usize> Rem<&NonZero<Uint<LIMBS>>> for &Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn rem(self, rhs: &NonZero<Uint<LIMBS>>) -> Self::Output {
        *self % *rhs
    }
}

impl<const LIMBS: usize> Rem<&NonZero<Uint<LIMBS>>> for Wrapping<Uint<LIMBS>> {
    type Output = Wrapping<Uint<LIMBS>>;

    fn rem(self, rhs: &NonZero<Uint<LIMBS>>) -> Self::Output {
        self % *rhs
    }
}

impl<const LIMBS: usize> RemAssign<NonZero<Uint<LIMBS>>> for Wrapping<Uint<LIMBS>> {
    fn rem_assign(&mut self, rhs: NonZero<Uint<LIMBS>>) {
        *self %= &rhs;
    }
}

impl<const LIMBS: usize> RemAssign<&NonZero<Uint<LIMBS>>> for Wrapping<Uint<LIMBS>> {
    fn rem_assign(&mut self, rhs: &NonZero<Uint<LIMBS>>) {
        *self = Wrapping(self.0 % rhs)
    }
}

#[cfg(test)]
#[path = "div_tests.rs"]
mod tests;
