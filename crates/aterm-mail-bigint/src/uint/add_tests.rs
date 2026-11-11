// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{CheckedAdd, Limb, U128};

#[test]
fn adc_no_carry() {
    let (res, carry) = U128::ZERO.adc(&U128::ONE, Limb::ZERO);
    assert_eq!(res, U128::ONE);
    assert_eq!(carry, Limb::ZERO);
}

#[test]
fn adc_with_carry() {
    let (res, carry) = U128::MAX.adc(&U128::ONE, Limb::ZERO);
    assert_eq!(res, U128::ZERO);
    assert_eq!(carry, Limb::ONE);
}

#[test]
fn saturating_add_no_carry() {
    assert_eq!(U128::ZERO.saturating_add(&U128::ONE), U128::ONE);
}

#[test]
fn saturating_add_with_carry() {
    assert_eq!(U128::MAX.saturating_add(&U128::ONE), U128::MAX);
}

#[test]
fn wrapping_add_no_carry() {
    assert_eq!(U128::ZERO.wrapping_add(&U128::ONE), U128::ONE);
}

#[test]
fn wrapping_add_with_carry() {
    assert_eq!(U128::MAX.wrapping_add(&U128::ONE), U128::ZERO);
}

#[test]
fn checked_add_ok() {
    let result = U128::ZERO.checked_add(&U128::ONE);
    assert_eq!(result.unwrap(), U128::ONE);
}

#[test]
fn checked_add_overflow() {
    let result = U128::MAX.checked_add(&U128::ONE);
    assert!(!bool::from(result.is_some()));
}
