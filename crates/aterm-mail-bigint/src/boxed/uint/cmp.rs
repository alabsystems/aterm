// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`BoxedUint`] comparisons.
//!
//! By default these are all constant-time and use the `subtle` crate.

use super::BoxedUint;
use crate::Limb;
use subtle::{Choice, ConstantTimeEq};

impl ConstantTimeEq for BoxedUint {
    #[inline]
    fn ct_eq(&self, other: &Self) -> Choice {
        let (shorter, longer) = Self::sort_by_precision(self, other);
        let mut ret = Choice::from(1u8);

        for i in 0..longer.limbs.len() {
            let a = shorter.limbs.get(i).unwrap_or(&Limb::ZERO);
            let b = longer.limbs.get(i).unwrap_or(&Limb::ZERO);
            ret &= a.ct_eq(b);
        }

        ret
    }
}

impl Eq for BoxedUint {}
impl PartialEq for BoxedUint {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).into()
    }
}

#[cfg(test)]
#[path = "cmp_tests.rs"]
mod tests;
