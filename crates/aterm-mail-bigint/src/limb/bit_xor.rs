// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Limb bit xor operations.

use super::Limb;
use core::ops::BitXor;

impl Limb {
    /// Calculates `a ^ b`.
    pub const fn bitxor(self, rhs: Self) -> Self {
        Limb(self.0 ^ rhs.0)
    }
}

impl BitXor for Limb {
    type Output = Limb;

    fn bitxor(self, rhs: Self) -> Self::Output {
        self.bitxor(rhs)
    }
}
