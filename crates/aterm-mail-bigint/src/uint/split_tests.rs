// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{U64, U128};

#[test]
fn split() {
    let (hi, lo) = U128::from_be_hex("00112233445566778899aabbccddeeff").split();
    assert_eq!(hi, U64::from_u64(0x0011223344556677));
    assert_eq!(lo, U64::from_u64(0x8899aabbccddeeff));
}
