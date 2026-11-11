// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{CheckedSub, Limb, U128};

#[test]
fn sbb_no_borrow() {
    let (res, borrow) = U128::ONE.sbb(&U128::ONE, Limb::ZERO);
    assert_eq!(res, U128::ZERO);
    assert_eq!(borrow, Limb::ZERO);
}

#[test]
fn sbb_with_borrow() {
    let (res, borrow) = U128::ZERO.sbb(&U128::ONE, Limb::ZERO);

    assert_eq!(res, U128::MAX);
    assert_eq!(borrow, Limb::MAX);
}

#[test]
fn saturating_sub_no_borrow() {
    assert_eq!(
        U128::from(5u64).saturating_sub(&U128::ONE),
        U128::from(4u64)
    );
}

#[test]
fn saturating_sub_with_borrow() {
    assert_eq!(
        U128::from(4u64).saturating_sub(&U128::from(5u64)),
        U128::ZERO
    );
}

#[test]
fn wrapping_sub_no_borrow() {
    assert_eq!(U128::ONE.wrapping_sub(&U128::ONE), U128::ZERO);
}

#[test]
fn wrapping_sub_with_borrow() {
    assert_eq!(U128::ZERO.wrapping_sub(&U128::ONE), U128::MAX);
}

#[test]
fn checked_sub_ok() {
    let result = U128::ONE.checked_sub(&U128::ONE);
    assert_eq!(result.unwrap(), U128::ZERO);
}

#[test]
fn checked_sub_overflow() {
    let result = U128::ZERO.checked_sub(&U128::ONE);
    assert!(!bool::from(result.is_some()));
}
