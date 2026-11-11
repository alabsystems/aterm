// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use core::ops::{Sub, SubAssign};

use crate::modular::sub::sub_montgomery_form;

use super::{Residue, ResidueParams};

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Residue<MOD, LIMBS> {
    /// Subtracts `rhs`.
    pub const fn sub(&self, rhs: &Self) -> Self {
        Self {
            montgomery_form: sub_montgomery_form(
                &self.montgomery_form,
                &rhs.montgomery_form,
                &MOD::MODULUS,
            ),
            phantom: core::marker::PhantomData,
        }
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Sub<&Residue<MOD, LIMBS>>
    for &Residue<MOD, LIMBS>
{
    type Output = Residue<MOD, LIMBS>;
    fn sub(self, rhs: &Residue<MOD, LIMBS>) -> Residue<MOD, LIMBS> {
        self.sub(rhs)
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Sub<Residue<MOD, LIMBS>>
    for &Residue<MOD, LIMBS>
{
    type Output = Residue<MOD, LIMBS>;
    #[allow(clippy::op_ref)]
    fn sub(self, rhs: Residue<MOD, LIMBS>) -> Residue<MOD, LIMBS> {
        self - &rhs
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Sub<&Residue<MOD, LIMBS>>
    for Residue<MOD, LIMBS>
{
    type Output = Residue<MOD, LIMBS>;
    #[allow(clippy::op_ref)]
    fn sub(self, rhs: &Residue<MOD, LIMBS>) -> Residue<MOD, LIMBS> {
        &self - rhs
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Sub<Residue<MOD, LIMBS>>
    for Residue<MOD, LIMBS>
{
    type Output = Residue<MOD, LIMBS>;
    fn sub(self, rhs: Residue<MOD, LIMBS>) -> Residue<MOD, LIMBS> {
        &self - &rhs
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> SubAssign<&Self> for Residue<MOD, LIMBS> {
    fn sub_assign(&mut self, rhs: &Self) {
        *self = *self - rhs;
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> SubAssign<Self> for Residue<MOD, LIMBS> {
    fn sub_assign(&mut self, rhs: Self) {
        *self -= &rhs;
    }
}

#[cfg(test)]
#[path = "const_sub_tests.rs"]
mod tests;
