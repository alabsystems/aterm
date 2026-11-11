// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;

const LIMBS: usize = nlimbs!(64);

#[test]
#[allow(deprecated)]
// Test that a valid modulus yields `DynResidueParams`
fn test_valid_modulus() {
    let valid_modulus = Uint::<LIMBS>::from(3u8);

    DynResidueParams::<LIMBS>::new_checked(&valid_modulus).unwrap();
    DynResidueParams::<LIMBS>::new(&valid_modulus);
}

#[test]
#[allow(deprecated)]
// Test that an invalid checked modulus does not yield `DynResidueParams`
fn test_invalid_checked_modulus() {
    assert!(bool::from(
        DynResidueParams::<LIMBS>::new_checked(&Uint::from(2u8)).is_none()
    ))
}

#[test]
#[should_panic]
// Tets that an invalid modulus panics
fn test_invalid_modulus() {
    DynResidueParams::<LIMBS>::new(&Uint::from(2u8));
}
