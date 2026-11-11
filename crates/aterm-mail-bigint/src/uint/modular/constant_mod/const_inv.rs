// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use core::marker::PhantomData;

use subtle::CtOption;

use crate::{CtChoice, NonZero, modular::inv::inv_montgomery_form, traits::Invert};

use super::{Residue, ResidueParams};

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Residue<MOD, LIMBS> {
    /// Computes the residue `self^-1` representing the multiplicative inverse of `self`.
    /// I.e. `self * self^-1 = 1`.
    /// If the number was invertible, the second element of the tuple is the truthy value,
    /// otherwise it is the falsy value (in which case the first element's value is unspecified).
    pub const fn invert(&self) -> (Self, CtChoice) {
        let (montgomery_form, is_some) = inv_montgomery_form(
            &self.montgomery_form,
            &MOD::MODULUS,
            &MOD::R3,
            MOD::MOD_NEG_INV,
        );

        let value = Self {
            montgomery_form,
            phantom: PhantomData,
        };

        (value, is_some)
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Invert for Residue<MOD, LIMBS> {
    type Output = CtOption<Self>;
    fn invert(&self) -> Self::Output {
        let (value, is_some) = self.invert();
        CtOption::new(value, is_some.into())
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Invert for NonZero<Residue<MOD, LIMBS>> {
    type Output = Self;
    fn invert(&self) -> Self::Output {
        // Always succeeds for a non-zero argument
        let (value, _is_some) = self.as_ref().invert();
        NonZero::new(value).unwrap()
    }
}

#[cfg(test)]
#[path = "const_inv_tests.rs"]
mod tests;
