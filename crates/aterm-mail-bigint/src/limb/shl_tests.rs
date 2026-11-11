// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::Limb;

#[test]
fn shl1() {
    assert_eq!(Limb(1) << 1, Limb(2));
}

#[test]
fn shl2() {
    assert_eq!(Limb(1) << 2, Limb(4));
}

#[test]
fn shl_assign1() {
    let mut l = Limb(1);
    l <<= 1;
    assert_eq!(l, Limb(2));
}

#[test]
fn shl_assign2() {
    let mut l = Limb(1);
    l <<= 2;
    assert_eq!(l, Limb(4));
}
