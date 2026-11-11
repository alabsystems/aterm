// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`Uint`] multiplication modulus operations.

use crate::{Limb, Uint, WideWord, Word};

impl<const LIMBS: usize> Uint<LIMBS> {
    /// Computes `self * rhs mod p` for the special modulus
    /// `p = MAX+1-c` where `c` is small enough to fit in a single [`Limb`].
    /// For the modulus reduction, this function implements Algorithm 14.47 from
    /// the "Handbook of Applied Cryptography", by A. Menezes, P. van Oorschot,
    /// and S. Vanstone, CRC Press, 1996.
    pub const fn mul_mod_special(&self, rhs: &Self, c: Limb) -> Self {
        // We implicitly assume `LIMBS > 0`, because `Uint<0>` doesn't compile.
        // Still the case `LIMBS == 1` needs special handling.
        if LIMBS == 1 {
            let prod = self.limbs[0].0 as WideWord * rhs.limbs[0].0 as WideWord;
            let reduced = prod % Word::MIN.wrapping_sub(c.0) as WideWord;
            return Self::from_word(reduced as Word);
        }

        let (lo, hi) = self.mul_wide(rhs);

        // Now use Algorithm 14.47 for the reduction
        let (lo, carry) = mac_by_limb(&lo, &hi, c, Limb::ZERO);

        let (lo, carry) = {
            let rhs = (carry.0 + 1) as WideWord * c.0 as WideWord;
            lo.adc(&Self::from_wide_word(rhs), Limb::ZERO)
        };

        let (lo, _) = {
            let rhs = carry.0.wrapping_sub(1) & c.0;
            lo.sbb(&Self::from_word(rhs), Limb::ZERO)
        };

        lo
    }
}

/// Computes `a + (b * c) + carry`, returning the result along with the new carry.
const fn mac_by_limb<const LIMBS: usize>(
    a: &Uint<LIMBS>,
    b: &Uint<LIMBS>,
    c: Limb,
    carry: Limb,
) -> (Uint<LIMBS>, Limb) {
    let mut i = 0;
    let mut a = *a;
    let mut carry = carry;

    while i < LIMBS {
        let (n, c) = a.limbs[i].mac(b.limbs[i], c, carry);
        a.limbs[i] = n;
        carry = c;
        i += 1;
    }

    (a, carry)
}

#[cfg(all(test, feature = "rand"))]
#[path = "mul_mod_tests.rs"]
mod tests;
