// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::Limb;

#[test]
fn shr1() {
    assert_eq!(Limb(2) >> 1, Limb(1));
}

#[test]
fn shr2() {
    assert_eq!(Limb(16) >> 2, Limb(4));
}

#[test]
fn shr_assign1() {
    let mut l = Limb::ONE;
    l >>= 1;
    assert_eq!(l, Limb::ZERO);
}

#[test]
fn shr_assign2() {
    let mut l = Limb(32);
    l >>= 2;
    assert_eq!(l, Limb(8));
}
