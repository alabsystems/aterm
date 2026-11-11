// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Limb multiplication

use crate::{Checked, CheckedMul, Limb, WideWord, Word, Wrapping, Zero};
use core::ops::{Mul, MulAssign};
use subtle::CtOption;

impl Limb {
    /// Computes `self + (b * c) + carry`, returning the result along with the new carry.
    #[inline(always)]
    pub const fn mac(self, b: Limb, c: Limb, carry: Limb) -> (Limb, Limb) {
        let a = self.0 as WideWord;
        let b = b.0 as WideWord;
        let c = c.0 as WideWord;
        let carry = carry.0 as WideWord;
        let ret = a + (b * c) + carry;
        (Limb(ret as Word), Limb((ret >> Self::BITS) as Word))
    }

    /// Perform saturating multiplication.
    #[inline]
    pub const fn saturating_mul(&self, rhs: Self) -> Self {
        Limb(self.0.saturating_mul(rhs.0))
    }

    /// Perform wrapping multiplication, discarding overflow.
    #[inline(always)]
    pub const fn wrapping_mul(&self, rhs: Self) -> Self {
        Limb(self.0.wrapping_mul(rhs.0))
    }

    /// Compute "wide" multiplication, with a product twice the size of the input.
    pub(crate) const fn mul_wide(&self, rhs: Self) -> WideWord {
        (self.0 as WideWord) * (rhs.0 as WideWord)
    }
}

impl CheckedMul for Limb {
    type Output = Self;

    #[inline]
    fn checked_mul(&self, rhs: Self) -> CtOption<Self> {
        let result = self.mul_wide(rhs);
        let overflow = Limb((result >> Self::BITS) as Word);
        CtOption::new(Limb(result as Word), overflow.is_zero())
    }
}

impl Mul for Wrapping<Limb> {
    type Output = Self;

    fn mul(self, rhs: Self) -> Wrapping<Limb> {
        Wrapping(self.0.wrapping_mul(rhs.0))
    }
}

impl Mul<&Wrapping<Limb>> for Wrapping<Limb> {
    type Output = Wrapping<Limb>;

    fn mul(self, rhs: &Wrapping<Limb>) -> Wrapping<Limb> {
        Wrapping(self.0.wrapping_mul(rhs.0))
    }
}

impl Mul<Wrapping<Limb>> for &Wrapping<Limb> {
    type Output = Wrapping<Limb>;

    fn mul(self, rhs: Wrapping<Limb>) -> Wrapping<Limb> {
        Wrapping(self.0.wrapping_mul(rhs.0))
    }
}

impl Mul<&Wrapping<Limb>> for &Wrapping<Limb> {
    type Output = Wrapping<Limb>;

    fn mul(self, rhs: &Wrapping<Limb>) -> Wrapping<Limb> {
        Wrapping(self.0.wrapping_mul(rhs.0))
    }
}

impl MulAssign for Wrapping<Limb> {
    fn mul_assign(&mut self, other: Self) {
        *self = *self * other;
    }
}

impl MulAssign<&Wrapping<Limb>> for Wrapping<Limb> {
    fn mul_assign(&mut self, other: &Self) {
        *self = *self * other;
    }
}

impl Mul for Checked<Limb> {
    type Output = Self;

    fn mul(self, rhs: Self) -> Checked<Limb> {
        Checked(
            self.0
                .and_then(|lhs| rhs.0.and_then(|rhs| lhs.checked_mul(rhs))),
        )
    }
}

impl Mul<&Checked<Limb>> for Checked<Limb> {
    type Output = Checked<Limb>;

    fn mul(self, rhs: &Checked<Limb>) -> Checked<Limb> {
        Checked(
            self.0
                .and_then(|lhs| rhs.0.and_then(|rhs| lhs.checked_mul(rhs))),
        )
    }
}

impl Mul<Checked<Limb>> for &Checked<Limb> {
    type Output = Checked<Limb>;

    fn mul(self, rhs: Checked<Limb>) -> Checked<Limb> {
        Checked(
            self.0
                .and_then(|lhs| rhs.0.and_then(|rhs| lhs.checked_mul(rhs))),
        )
    }
}

impl Mul<&Checked<Limb>> for &Checked<Limb> {
    type Output = Checked<Limb>;

    fn mul(self, rhs: &Checked<Limb>) -> Checked<Limb> {
        Checked(
            self.0
                .and_then(|lhs| rhs.0.and_then(|rhs| lhs.checked_mul(rhs))),
        )
    }
}

impl MulAssign for Checked<Limb> {
    fn mul_assign(&mut self, other: Self) {
        *self = *self * other;
    }
}

impl MulAssign<&Checked<Limb>> for Checked<Limb> {
    fn mul_assign(&mut self, other: &Self) {
        *self = *self * other;
    }
}

#[cfg(test)]
#[path = "mul_tests.rs"]
mod tests;
