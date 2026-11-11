// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{CheckedAdd, Limb};

#[test]
fn adc_no_carry() {
    let (res, carry) = Limb::ZERO.adc(Limb::ONE, Limb::ZERO);
    assert_eq!(res, Limb::ONE);
    assert_eq!(carry, Limb::ZERO);
}

#[test]
fn adc_with_carry() {
    let (res, carry) = Limb::MAX.adc(Limb::ONE, Limb::ZERO);
    assert_eq!(res, Limb::ZERO);
    assert_eq!(carry, Limb::ONE);
}

#[test]
fn wrapping_add_no_carry() {
    assert_eq!(Limb::ZERO.wrapping_add(Limb::ONE), Limb::ONE);
}

#[test]
fn wrapping_add_with_carry() {
    assert_eq!(Limb::MAX.wrapping_add(Limb::ONE), Limb::ZERO);
}

#[test]
fn checked_add_ok() {
    let result = Limb::ZERO.checked_add(Limb::ONE);
    assert_eq!(result.unwrap(), Limb::ONE);
}

#[test]
fn checked_add_overflow() {
    let result = Limb::MAX.checked_add(Limb::ONE);
    assert!(!bool::from(result.is_some()));
}
