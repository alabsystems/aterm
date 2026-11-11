// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::BoxedUint;
use subtle::ConstantTimeEq;

#[test]
fn ct_eq() {
    let a = BoxedUint::zero();
    let b = BoxedUint::one();

    assert!(bool::from(a.ct_eq(&a)));
    assert!(!bool::from(a.ct_eq(&b)));
    assert!(!bool::from(b.ct_eq(&a)));
    assert!(bool::from(b.ct_eq(&b)));
}
