// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Adapted encoding tests to deterministic cases without upstream test packages.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Limb encoding

use super::{Limb, Word};
use crate::Encoding;

impl Encoding for Limb {
    #[cfg(target_pointer_width = "32")]
    type Repr = [u8; 4];
    #[cfg(target_pointer_width = "64")]
    type Repr = [u8; 8];

    #[inline]
    fn from_be_bytes(bytes: Self::Repr) -> Self {
        Limb(Word::from_be_bytes(bytes))
    }

    #[inline]
    fn from_le_bytes(bytes: Self::Repr) -> Self {
        Limb(Word::from_le_bytes(bytes))
    }

    #[inline]
    fn to_be_bytes(&self) -> Self::Repr {
        self.0.to_be_bytes()
    }

    #[inline]
    fn to_le_bytes(&self) -> Self::Repr {
        self.0.to_le_bytes()
    }
}

#[cfg(test)]
#[path = "encoding_test_tests.rs"]
mod test;
