// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{U64, U128};

#[test]
fn resize_larger() {
    let u = U64::from_be_hex("AAAAAAAABBBBBBBB");
    let u2: U128 = u.resize();
    assert_eq!(u2, U128::from_be_hex("0000000000000000AAAAAAAABBBBBBBB"));
}

#[test]
fn resize_smaller() {
    let u = U128::from_be_hex("AAAAAAAABBBBBBBBCCCCCCCCDDDDDDDD");
    let u2: U64 = u.resize();
    assert_eq!(u2, U64::from_be_hex("CCCCCCCCDDDDDDDD"));
}
