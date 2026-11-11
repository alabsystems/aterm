// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use core::ops::{Sub, SubAssign};

use crate::modular::sub::sub_montgomery_form;

use super::DynResidue;

impl<const LIMBS: usize> DynResidue<LIMBS> {
    /// Subtracts `rhs`.
    pub const fn sub(&self, rhs: &Self) -> Self {
        Self {
            montgomery_form: sub_montgomery_form(
                &self.montgomery_form,
                &rhs.montgomery_form,
                &self.residue_params.modulus,
            ),
            residue_params: self.residue_params,
        }
    }
}

impl<const LIMBS: usize> Sub<&DynResidue<LIMBS>> for &DynResidue<LIMBS> {
    type Output = DynResidue<LIMBS>;
    fn sub(self, rhs: &DynResidue<LIMBS>) -> DynResidue<LIMBS> {
        debug_assert_eq!(self.residue_params, rhs.residue_params);
        self.sub(rhs)
    }
}

impl<const LIMBS: usize> Sub<DynResidue<LIMBS>> for &DynResidue<LIMBS> {
    type Output = DynResidue<LIMBS>;
    #[allow(clippy::op_ref)]
    fn sub(self, rhs: DynResidue<LIMBS>) -> DynResidue<LIMBS> {
        self - &rhs
    }
}

impl<const LIMBS: usize> Sub<&DynResidue<LIMBS>> for DynResidue<LIMBS> {
    type Output = DynResidue<LIMBS>;
    #[allow(clippy::op_ref)]
    fn sub(self, rhs: &DynResidue<LIMBS>) -> DynResidue<LIMBS> {
        &self - rhs
    }
}

impl<const LIMBS: usize> Sub<DynResidue<LIMBS>> for DynResidue<LIMBS> {
    type Output = DynResidue<LIMBS>;
    fn sub(self, rhs: DynResidue<LIMBS>) -> DynResidue<LIMBS> {
        &self - &rhs
    }
}

impl<const LIMBS: usize> SubAssign<&DynResidue<LIMBS>> for DynResidue<LIMBS> {
    fn sub_assign(&mut self, rhs: &DynResidue<LIMBS>) {
        *self = *self - rhs;
    }
}

impl<const LIMBS: usize> SubAssign<DynResidue<LIMBS>> for DynResidue<LIMBS> {
    fn sub_assign(&mut self, rhs: DynResidue<LIMBS>) {
        *self -= &rhs;
    }
}

#[cfg(test)]
#[path = "runtime_sub_tests.rs"]
mod tests;
