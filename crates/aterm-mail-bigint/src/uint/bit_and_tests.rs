// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::U128;

#[test]
fn checked_and_ok() {
    let result = U128::ZERO.checked_and(&U128::ONE);
    assert_eq!(result.unwrap(), U128::ZERO);
}

#[test]
fn overlapping_and_ok() {
    let result = U128::MAX.wrapping_and(&U128::ONE);
    assert_eq!(result, U128::ONE);
}
