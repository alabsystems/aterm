// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{CheckedSub, Limb};

#[test]
fn sbb_no_borrow() {
    let (res, borrow) = Limb::ONE.sbb(Limb::ONE, Limb::ZERO);
    assert_eq!(res, Limb::ZERO);
    assert_eq!(borrow, Limb::ZERO);
}

#[test]
fn sbb_with_borrow() {
    let (res, borrow) = Limb::ZERO.sbb(Limb::ONE, Limb::ZERO);

    assert_eq!(res, Limb::MAX);
    assert_eq!(borrow, Limb::MAX);
}

#[test]
fn wrapping_sub_no_borrow() {
    assert_eq!(Limb::ONE.wrapping_sub(Limb::ONE), Limb::ZERO);
}

#[test]
fn wrapping_sub_with_borrow() {
    assert_eq!(Limb::ZERO.wrapping_sub(Limb::ONE), Limb::MAX);
}

#[test]
fn checked_sub_ok() {
    let result = Limb::ONE.checked_sub(Limb::ONE);
    assert_eq!(result.unwrap(), Limb::ZERO);
}

#[test]
fn checked_sub_overflow() {
    let result = Limb::ZERO.checked_sub(Limb::ONE);
    assert!(!bool::from(result.is_some()));
}
