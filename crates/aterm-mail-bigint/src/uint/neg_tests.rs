// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::U256;

#[test]
fn wrapping_neg() {
    assert_eq!(U256::ZERO.wrapping_neg(), U256::ZERO);
    assert_eq!(U256::MAX.wrapping_neg(), U256::ONE);
    assert_eq!(
        U256::from_u64(13).wrapping_neg(),
        U256::from_u64(13).not().saturating_add(&U256::ONE)
    );
    assert_eq!(
        U256::from_u64(42).wrapping_neg(),
        U256::from_u64(42).saturating_sub(&U256::ONE).not()
    );
}
