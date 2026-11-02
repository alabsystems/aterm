// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use core::ops::Neg;

use super::DynResidue;

impl<const LIMBS: usize> DynResidue<LIMBS> {
    /// Negates the number.
    pub const fn neg(&self) -> Self {
        Self::zero(self.residue_params).sub(self)
    }
}

impl<const LIMBS: usize> Neg for DynResidue<LIMBS> {
    type Output = Self;
    fn neg(self) -> Self {
        DynResidue::neg(&self)
    }
}

impl<const LIMBS: usize> Neg for &DynResidue<LIMBS> {
    type Output = DynResidue<LIMBS>;
    fn neg(self) -> DynResidue<LIMBS> {
        DynResidue::neg(self)
    }
}
