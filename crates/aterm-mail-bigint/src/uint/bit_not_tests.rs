// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::U128;

#[test]
fn bitnot_ok() {
    assert_eq!(U128::ZERO.not(), U128::MAX);
    assert_eq!(U128::MAX.not(), U128::ZERO);
}
