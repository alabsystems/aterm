// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Random number generator support

use super::Limb;
use crate::{Encoding, NonZero, Random, RandomMod};
use rand_core::CryptoRngCore;
use subtle::ConstantTimeLess;

impl Random for Limb {
    #[cfg(target_pointer_width = "32")]
    fn random(rng: &mut impl CryptoRngCore) -> Self {
        Self(rng.next_u32())
    }

    #[cfg(target_pointer_width = "64")]
    fn random(rng: &mut impl CryptoRngCore) -> Self {
        Self(rng.next_u64())
    }
}

impl RandomMod for Limb {
    fn random_mod(rng: &mut impl CryptoRngCore, modulus: &NonZero<Self>) -> Self {
        let mut bytes = <Self as Encoding>::Repr::default();

        let n_bits = modulus.bits();
        let n_bytes = (n_bits + 7) / 8;
        let mask = 0xff >> (8 * n_bytes - n_bits);

        loop {
            rng.fill_bytes(&mut bytes[..n_bytes]);
            bytes[n_bytes - 1] &= mask;

            let n = Limb::from_le_bytes(bytes);
            if n.ct_lt(modulus).into() {
                return n;
            }
        }
    }
}
