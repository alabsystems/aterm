// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`Uint`] negation modulus operations.

use crate::{Limb, NegMod, Uint};

impl<const LIMBS: usize> Uint<LIMBS> {
    /// Computes `-a mod p`.
    /// Assumes `self` is in `[0, p)`.
    pub const fn neg_mod(&self, p: &Self) -> Self {
        let z = self.ct_is_nonzero();
        let mut ret = p.sbb(self, Limb::ZERO).0;
        let mut i = 0;
        while i < LIMBS {
            // Set ret to 0 if the original value was 0, in which
            // case ret would be p.
            ret.limbs[i].0 = z.if_true(ret.limbs[i].0);
            i += 1;
        }
        ret
    }

    /// Computes `-a mod p` for the special modulus
    /// `p = MAX+1-c` where `c` is small enough to fit in a single [`Limb`].
    pub const fn neg_mod_special(&self, c: Limb) -> Self {
        Self::ZERO.sub_mod_special(self, c)
    }
}

impl<const LIMBS: usize> NegMod for Uint<LIMBS> {
    type Output = Self;

    fn neg_mod(&self, p: &Self) -> Self {
        debug_assert!(self < p);
        self.neg_mod(p)
    }
}

#[cfg(test)]
#[path = "neg_mod_tests.rs"]
mod tests;
