// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use core::ops::Neg;

use super::{Residue, ResidueParams};

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Residue<MOD, LIMBS> {
    /// Negates the number.
    pub const fn neg(&self) -> Self {
        Self::ZERO.sub(self)
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Neg for Residue<MOD, LIMBS> {
    type Output = Self;
    fn neg(self) -> Self {
        Residue::neg(&self)
    }
}

impl<MOD: ResidueParams<LIMBS>, const LIMBS: usize> Neg for &Residue<MOD, LIMBS> {
    type Output = Residue<MOD, LIMBS>;
    fn neg(self) -> Residue<MOD, LIMBS> {
        Residue::neg(self)
    }
}

#[cfg(test)]
#[path = "const_neg_tests.rs"]
mod tests;
