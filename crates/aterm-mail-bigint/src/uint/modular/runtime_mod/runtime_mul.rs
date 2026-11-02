// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use core::ops::{Mul, MulAssign};

use crate::{
    modular::mul::{mul_montgomery_form, square_montgomery_form},
    traits::Square,
};

use super::DynResidue;

impl<const LIMBS: usize> DynResidue<LIMBS> {
    /// Multiplies by `rhs`.
    pub const fn mul(&self, rhs: &Self) -> Self {
        Self {
            montgomery_form: mul_montgomery_form(
                &self.montgomery_form,
                &rhs.montgomery_form,
                &self.residue_params.modulus,
                self.residue_params.mod_neg_inv,
            ),
            residue_params: self.residue_params,
        }
    }

    /// Computes the (reduced) square of a residue.
    pub const fn square(&self) -> Self {
        Self {
            montgomery_form: square_montgomery_form(
                &self.montgomery_form,
                &self.residue_params.modulus,
                self.residue_params.mod_neg_inv,
            ),
            residue_params: self.residue_params,
        }
    }
}

impl<const LIMBS: usize> Mul<&DynResidue<LIMBS>> for &DynResidue<LIMBS> {
    type Output = DynResidue<LIMBS>;
    fn mul(self, rhs: &DynResidue<LIMBS>) -> DynResidue<LIMBS> {
        debug_assert_eq!(self.residue_params, rhs.residue_params);
        self.mul(rhs)
    }
}

impl<const LIMBS: usize> Mul<DynResidue<LIMBS>> for &DynResidue<LIMBS> {
    type Output = DynResidue<LIMBS>;
    #[allow(clippy::op_ref)]
    fn mul(self, rhs: DynResidue<LIMBS>) -> DynResidue<LIMBS> {
        self * &rhs
    }
}

impl<const LIMBS: usize> Mul<&DynResidue<LIMBS>> for DynResidue<LIMBS> {
    type Output = DynResidue<LIMBS>;
    #[allow(clippy::op_ref)]
    fn mul(self, rhs: &DynResidue<LIMBS>) -> DynResidue<LIMBS> {
        &self * rhs
    }
}

impl<const LIMBS: usize> Mul<DynResidue<LIMBS>> for DynResidue<LIMBS> {
    type Output = DynResidue<LIMBS>;
    fn mul(self, rhs: DynResidue<LIMBS>) -> DynResidue<LIMBS> {
        &self * &rhs
    }
}

impl<const LIMBS: usize> MulAssign<&DynResidue<LIMBS>> for DynResidue<LIMBS> {
    fn mul_assign(&mut self, rhs: &DynResidue<LIMBS>) {
        *self = *self * rhs;
    }
}

impl<const LIMBS: usize> MulAssign<DynResidue<LIMBS>> for DynResidue<LIMBS> {
    fn mul_assign(&mut self, rhs: DynResidue<LIMBS>) {
        *self *= &rhs;
    }
}

impl<const LIMBS: usize> Square for DynResidue<LIMBS> {
    fn square(&self) -> Self {
        DynResidue::square(self)
    }
}
