// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Limb bit not operations.

use super::Limb;
use core::ops::Not;

impl Limb {
    /// Calculates `!a`.
    pub const fn not(self) -> Self {
        Limb(!self.0)
    }
}

impl Not for Limb {
    type Output = Limb;

    fn not(self) -> <Self as Not>::Output {
        self.not()
    }
}
