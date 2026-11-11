// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Limb, Uint};

/// Split this number in half, returning its high and low components
/// respectively.
#[inline]
pub(crate) const fn split_mixed<const L: usize, const H: usize, const O: usize>(
    n: &Uint<O>,
) -> (Uint<H>, Uint<L>) {
    let top = L + H;
    let top = if top < O { top } else { O };
    let mut lo = [Limb::ZERO; L];
    let mut hi = [Limb::ZERO; H];
    let mut i = 0;

    while i < top {
        if i < L {
            lo[i] = n.limbs[i];
        } else {
            hi[i - L] = n.limbs[i];
        }
        i += 1;
    }

    (Uint { limbs: hi }, Uint { limbs: lo })
}

#[cfg(test)]
#[path = "split_tests.rs"]
mod tests;
