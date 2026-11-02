// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{CtChoice, Limb, Uint, modular::reduction::montgomery_reduction};

pub const fn inv_montgomery_form<const LIMBS: usize>(
    x: &Uint<LIMBS>,
    modulus: &Uint<LIMBS>,
    r3: &Uint<LIMBS>,
    mod_neg_inv: Limb,
) -> (Uint<LIMBS>, CtChoice) {
    let (inverse, is_some) = x.inv_odd_mod(modulus);
    (
        montgomery_reduction(&inverse.mul_wide(r3), modulus, mod_neg_inv),
        is_some,
    )
}
