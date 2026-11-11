// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{MultiExponentiateBoundedExp, PowBoundedExp, Uint, modular::pow::pow_montgomery_form};

use super::{Residue, ResidueParams};
use crate::modular::pow::multi_exponentiate_montgomery_form_array;
#[cfg(feature = "alloc")]
use crate::modular::pow::multi_exponentiate_montgomery_form_slice;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Residue<MOD, LIMBS> {
    /// Raises to the `exponent` power.
    pub const fn pow<const RHS_LIMBS: usize>(
        &self,
        exponent: &Uint<RHS_LIMBS>,
    ) -> Residue<MOD, LIMBS> {
        self.pow_bounded_exp(exponent, Uint::<RHS_LIMBS>::BITS)
    }

    /// Raises to the `exponent` power,
    /// with `exponent_bits` representing the number of (least significant) bits
    /// to take into account for the exponent.
    ///
    /// NOTE: `exponent_bits` may be leaked in the time pattern.
    pub const fn pow_bounded_exp<const RHS_LIMBS: usize>(
        &self,
        exponent: &Uint<RHS_LIMBS>,
        exponent_bits: usize,
    ) -> Residue<MOD, LIMBS> {
        Self {
            montgomery_form: pow_montgomery_form(
                &self.montgomery_form,
                exponent,
                exponent_bits,
                &MOD::MODULUS,
                &MOD::R,
                MOD::MOD_NEG_INV,
            ),
            phantom: core::marker::PhantomData,
        }
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize, const RHS_LIMBS: usize>
    PowBoundedExp<Uint<RHS_LIMBS>> for Residue<MOD, LIMBS>
{
    fn pow_bounded_exp(&self, exponent: &Uint<RHS_LIMBS>, exponent_bits: usize) -> Self {
        self.pow_bounded_exp(exponent, exponent_bits)
    }
}

impl<const N: usize, MOD: ResidueParams<LIMBS>, const LIMBS: usize, const RHS_LIMBS: usize>
    MultiExponentiateBoundedExp<Uint<RHS_LIMBS>, [(Self, Uint<RHS_LIMBS>); N]>
    for Residue<MOD, LIMBS>
{
    fn multi_exponentiate_bounded_exp(
        bases_and_exponents: &[(Self, Uint<RHS_LIMBS>); N],
        exponent_bits: usize,
    ) -> Self {
        let mut bases_and_exponents_montgomery_form =
            [(Uint::<LIMBS>::ZERO, Uint::<RHS_LIMBS>::ZERO); N];

        let mut i = 0;
        while i < N {
            let (base, exponent) = bases_and_exponents[i];
            bases_and_exponents_montgomery_form[i] = (base.montgomery_form, exponent);
            i += 1;
        }

        Self {
            montgomery_form: multi_exponentiate_montgomery_form_array(
                &bases_and_exponents_montgomery_form,
                exponent_bits,
                &MOD::MODULUS,
                &MOD::R,
                MOD::MOD_NEG_INV,
            ),
            phantom: core::marker::PhantomData,
        }
    }
}

#[cfg(feature = "alloc")]
impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize, const RHS_LIMBS: usize>
    MultiExponentiateBoundedExp<Uint<RHS_LIMBS>, [(Self, Uint<RHS_LIMBS>)]>
    for Residue<MOD, LIMBS>
{
    fn multi_exponentiate_bounded_exp(
        bases_and_exponents: &[(Self, Uint<RHS_LIMBS>)],
        exponent_bits: usize,
    ) -> Self {
        let bases_and_exponents: Vec<(Uint<LIMBS>, Uint<RHS_LIMBS>)> = bases_and_exponents
            .iter()
            .map(|(base, exp)| (base.montgomery_form, *exp))
            .collect();
        Self {
            montgomery_form: multi_exponentiate_montgomery_form_slice(
                &bases_and_exponents,
                exponent_bits,
                &MOD::MODULUS,
                &MOD::R,
                MOD::MOD_NEG_INV,
            ),
            phantom: core::marker::PhantomData,
        }
    }
}

#[cfg(test)]
#[path = "const_pow_tests.rs"]
mod tests;
