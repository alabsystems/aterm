// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Limb, U128, Word};

#[cfg(target_pointer_width = "32")]
use crate::U64 as UintEx;

#[cfg(target_pointer_width = "64")]
use crate::U128 as UintEx;

#[test]
fn from_u8() {
    let n = UintEx::from(42u8);
    assert_eq!(n.as_limbs(), &[Limb(42), Limb(0)]);
}

#[test]
fn from_u16() {
    let n = UintEx::from(42u16);
    assert_eq!(n.as_limbs(), &[Limb(42), Limb(0)]);
}

#[test]
fn from_u64() {
    let n = UintEx::from(42u64);
    assert_eq!(n.as_limbs(), &[Limb(42), Limb(0)]);
}

#[test]
fn from_u128() {
    let n = U128::from(42u128);
    assert_eq!(&n.as_limbs()[..2], &[Limb(42), Limb(0)]);
    assert_eq!(u128::from(n), 42u128);
}

#[test]
fn array_round_trip() {
    let arr1 = [1, 2];
    let n = UintEx::from(arr1);
    let arr2: [Word; 2] = n.into();
    assert_eq!(arr1, arr2);
}
