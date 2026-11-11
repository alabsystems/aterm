// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{BoxedUint, CheckedAdd, Limb};

#[test]
fn adc_no_carry() {
    let (res, carry) = BoxedUint::zero().adc(&BoxedUint::one(), Limb::ZERO);
    assert_eq!(res, BoxedUint::one());
    assert_eq!(carry, Limb::ZERO);
}

#[test]
fn adc_with_carry() {
    let (res, carry) = BoxedUint::max(Limb::BITS)
        .unwrap()
        .adc(&BoxedUint::one(), Limb::ZERO);
    assert_eq!(res, BoxedUint::zero());
    assert_eq!(carry, Limb::ONE);
}

#[test]
fn checked_add_ok() {
    let result = BoxedUint::zero().checked_add(&BoxedUint::one());
    assert_eq!(result.unwrap(), BoxedUint::one());
}

#[test]
fn checked_add_overflow() {
    let result = BoxedUint::max(Limb::BITS)
        .unwrap()
        .checked_add(&BoxedUint::one());
    assert!(!bool::from(result.is_some()));
}
