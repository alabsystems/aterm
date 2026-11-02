// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Limb negation

use crate::{Limb, Wrapping};
use core::ops::Neg;

impl Neg for Wrapping<Limb> {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self(self.0.wrapping_neg())
    }
}

impl Limb {
    /// Perform wrapping negation.
    #[inline(always)]
    pub const fn wrapping_neg(self) -> Self {
        Limb(self.0.wrapping_neg())
    }
}
