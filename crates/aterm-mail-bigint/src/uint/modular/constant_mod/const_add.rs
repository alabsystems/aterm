// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use core::ops::{Add, AddAssign};

use crate::modular::add::add_montgomery_form;

use super::{Residue, ResidueParams};

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Residue<MOD, LIMBS> {
    /// Adds `rhs`.
    pub const fn add(&self, rhs: &Residue<MOD, LIMBS>) -> Self {
        Self {
            montgomery_form: add_montgomery_form(
                &self.montgomery_form,
                &rhs.montgomery_form,
                &MOD::MODULUS,
            ),
            phantom: core::marker::PhantomData,
        }
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Add<&Residue<MOD, LIMBS>>
    for &Residue<MOD, LIMBS>
{
    type Output = Residue<MOD, LIMBS>;
    fn add(self, rhs: &Residue<MOD, LIMBS>) -> Residue<MOD, LIMBS> {
        self.add(rhs)
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Add<Residue<MOD, LIMBS>>
    for &Residue<MOD, LIMBS>
{
    type Output = Residue<MOD, LIMBS>;
    #[allow(clippy::op_ref)]
    fn add(self, rhs: Residue<MOD, LIMBS>) -> Residue<MOD, LIMBS> {
        self + &rhs
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Add<&Residue<MOD, LIMBS>>
    for Residue<MOD, LIMBS>
{
    type Output = Residue<MOD, LIMBS>;
    #[allow(clippy::op_ref)]
    fn add(self, rhs: &Residue<MOD, LIMBS>) -> Residue<MOD, LIMBS> {
        &self + rhs
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Add<Residue<MOD, LIMBS>>
    for Residue<MOD, LIMBS>
{
    type Output = Residue<MOD, LIMBS>;
    fn add(self, rhs: Residue<MOD, LIMBS>) -> Residue<MOD, LIMBS> {
        &self + &rhs
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> AddAssign<&Self> for Residue<MOD, LIMBS> {
    fn add_assign(&mut self, rhs: &Self) {
        *self = *self + rhs;
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> AddAssign<Self> for Residue<MOD, LIMBS> {
    fn add_assign(&mut self, rhs: Self) {
        *self += &rhs;
    }
}

#[cfg(test)]
#[path = "const_add_tests.rs"]
mod tests;
