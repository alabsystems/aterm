// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::Uint;
use crate::CtChoice;

impl<const LIMBS: usize> Uint<LIMBS> {
    /// Computes 1/`self` mod `2^k`.
    /// This method is constant-time w.r.t. `self` but not `k`.
    ///
    /// Conditions: `self` < 2^k and `self` must be odd
    pub const fn inv_mod2k_vartime(&self, k: usize) -> Self {
        // Using the Algorithm 3 from "A Secure Algorithm for Inversion Modulo 2k"
        // by Sadiel de la Fe and Carles Ferrer.
        // See <https://www.mdpi.com/2410-387X/2/3/23>.

        // Note that we are not using Alrgorithm 4, since we have a different approach
        // of enforcing constant-timeness w.r.t. `self`.

        let mut x = Self::ZERO; // keeps `x` during iterations
        let mut b = Self::ONE; // keeps `b_i` during iterations
        let mut i = 0;

        while i < k {
            // X_i = b_i mod 2
            let x_i = b.limbs[0].0 & 1;
            let x_i_choice = CtChoice::from_lsb(x_i);
            // b_{i+1} = (b_i - a * X_i) / 2
            b = Self::ct_select(&b, &b.wrapping_sub(self), x_i_choice).shr_vartime(1);
            // Store the X_i bit in the result (x = x | (1 << X_i))
            x = x.bitor(&Uint::from_word(x_i).shl_vartime(i));

            i += 1;
        }

        x
    }

    /// Computes 1/`self` mod `2^k`.
    ///
    /// Conditions: `self` < 2^k and `self` must be odd
    pub const fn inv_mod2k(&self, k: usize) -> Self {
        // This is the same algorithm as in `inv_mod2k_vartime()`,
        // but made constant-time w.r.t `k` as well.

        let mut x = Self::ZERO; // keeps `x` during iterations
        let mut b = Self::ONE; // keeps `b_i` during iterations
        let mut i = 0;

        while i < Self::BITS {
            // Only iterations for i = 0..k need to change `x`,
            // the rest are dummy ones performed for the sake of constant-timeness.
            let within_range = CtChoice::from_usize_lt(i, k);

            // X_i = b_i mod 2
            let x_i = b.limbs[0].0 & 1;
            let x_i_choice = CtChoice::from_lsb(x_i);
            // b_{i+1} = (b_i - a * X_i) / 2
            b = Self::ct_select(&b, &b.wrapping_sub(self), x_i_choice).shr_vartime(1);

            // Store the X_i bit in the result (x = x | (1 << X_i))
            // Don't change the result in dummy iterations.
            let x_i_choice = x_i_choice.and(within_range);
            x = x.set_bit(i, x_i_choice);

            i += 1;
        }

        x
    }

    /// Computes the multiplicative inverse of `self` mod `modulus`, where `modulus` is odd.
    /// In other words `self^-1 mod modulus`.
    /// `bits` and `modulus_bits` are the bounds on the bit size
    /// of `self` and `modulus`, respectively
    /// (the inversion speed will be proportional to `bits + modulus_bits`).
    /// The second element of the tuple is the truthy value if an inverse exists,
    /// otherwise it is a falsy value.
    ///
    /// **Note:** variable time in `bits` and `modulus_bits`.
    ///
    /// The algorithm is the same as in GMP 6.2.1's `mpn_sec_invert`.
    pub const fn inv_odd_mod_bounded(
        &self,
        modulus: &Self,
        bits: usize,
        modulus_bits: usize,
    ) -> (Self, CtChoice) {
        debug_assert!(modulus.ct_is_odd().is_true_vartime());

        let mut a = *self;

        let mut u = Uint::ONE;
        let mut v = Uint::ZERO;

        let mut b = *modulus;

        // `bit_size` can be anything >= `self.bits()` + `modulus.bits()`, setting to the minimum.
        let bit_size = bits + modulus_bits;

        let mut m1hp = *modulus;
        let (m1hp_new, carry) = m1hp.shr_1();
        debug_assert!(carry.is_true_vartime());
        m1hp = m1hp_new.wrapping_add(&Uint::ONE);

        let mut i = 0;
        while i < bit_size {
            debug_assert!(b.ct_is_odd().is_true_vartime());

            let self_odd = a.ct_is_odd();

            // Set `self -= b` if `self` is odd.
            let (new_a, swap) = a.conditional_wrapping_sub(&b, self_odd);
            // Set `b += self` if `swap` is true.
            b = Uint::ct_select(&b, &b.wrapping_add(&new_a), swap);
            // Negate `self` if `swap` is true.
            a = new_a.conditional_wrapping_neg(swap);

            let (new_u, new_v) = Uint::ct_swap(&u, &v, swap);
            let (new_u, cy) = new_u.conditional_wrapping_sub(&new_v, self_odd);
            let (new_u, cyy) = new_u.conditional_wrapping_add(modulus, cy);
            debug_assert!(cy.is_true_vartime() == cyy.is_true_vartime());

            let (new_a, overflow) = a.shr_1();
            debug_assert!(!overflow.is_true_vartime());
            let (new_u, cy) = new_u.shr_1();
            let (new_u, cy) = new_u.conditional_wrapping_add(&m1hp, cy);
            debug_assert!(!cy.is_true_vartime());

            a = new_a;
            u = new_u;
            v = new_v;

            i += 1;
        }

        debug_assert!(!a.ct_is_nonzero().is_true_vartime());

        (v, Uint::ct_eq(&b, &Uint::ONE))
    }

    /// Computes the multiplicative inverse of `self` mod `modulus`, where `modulus` is odd.
    /// Returns `(inverse, CtChoice::TRUE)` if an inverse exists,
    /// otherwise `(undefined, CtChoice::FALSE)`.
    pub const fn inv_odd_mod(&self, modulus: &Self) -> (Self, CtChoice) {
        self.inv_odd_mod_bounded(modulus, Uint::<LIMBS>::BITS, Uint::<LIMBS>::BITS)
    }

    /// Computes the multiplicative inverse of `self` mod `modulus`.
    /// Returns `(inverse, CtChoice::TRUE)` if an inverse exists,
    /// otherwise `(undefined, CtChoice::FALSE)`.
    pub const fn inv_mod(&self, modulus: &Self) -> (Self, CtChoice) {
        // Decompose `modulus = s * 2^k` where `s` is odd
        let k = modulus.trailing_zeros();
        let s = modulus.shr(k);

        // Decompose `self` into RNS with moduli `2^k` and `s` and calculate the inverses.
        // Using the fact that `(z^{-1} mod (m1 * m2)) mod m1 == z^{-1} mod m1`
        let (a, a_is_some) = self.inv_odd_mod(&s);
        let b = self.inv_mod2k(k);
        // inverse modulo 2^k exists either if `k` is 0 or if `self` is odd.
        let b_is_some = CtChoice::from_usize_being_nonzero(k)
            .not()
            .or(self.ct_is_odd());

        // Restore from RNS:
        // self^{-1} = a mod s = b mod 2^k
        // => self^{-1} = a + s * ((b - a) * s^(-1) mod 2^k)
        // (essentially one step of the Garner's algorithm for recovery from RNS).

        let m_odd_inv = s.inv_mod2k(k); // `s` is odd, so this always exists

        // This part is mod 2^k
        let mask = Uint::ONE.shl(k).wrapping_sub(&Uint::ONE);
        let t = (b.wrapping_sub(&a).wrapping_mul(&m_odd_inv)).bitand(&mask);

        // Will not overflow since `a <= s - 1`, `t <= 2^k - 1`,
        // so `a + s * t <= s * 2^k - 1 == modulus - 1`.
        let result = a.wrapping_add(&s.wrapping_mul(&t));
        (result, a_is_some.and(b_is_some))
    }
}

#[cfg(test)]
#[path = "inv_mod_tests.rs"]
mod tests;
