// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::Limb;
use hex_literal::hex;

#[cfg(feature = "alloc")]
use {crate::U128, alloc::format};

#[cfg(target_pointer_width = "32")]
use crate::U64 as UintEx;

#[cfg(target_pointer_width = "64")]
use crate::U128 as UintEx;

#[test]
#[cfg(target_pointer_width = "32")]
fn from_be_slice() {
    let bytes = hex!("0011223344556677");
    let n = UintEx::from_be_slice(&bytes);
    assert_eq!(n.as_limbs(), &[Limb(0x44556677), Limb(0x00112233)]);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn from_be_slice() {
    let bytes = hex!("00112233445566778899aabbccddeeff");
    let n = UintEx::from_be_slice(&bytes);
    assert_eq!(
        n.as_limbs(),
        &[Limb(0x8899aabbccddeeff), Limb(0x0011223344556677)]
    );
}

#[test]
#[cfg(target_pointer_width = "32")]
fn from_le_slice() {
    let bytes = hex!("7766554433221100");
    let n = UintEx::from_le_slice(&bytes);
    assert_eq!(n.as_limbs(), &[Limb(0x44556677), Limb(0x00112233)]);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn from_le_slice() {
    let bytes = hex!("ffeeddccbbaa99887766554433221100");
    let n = UintEx::from_le_slice(&bytes);
    assert_eq!(
        n.as_limbs(),
        &[Limb(0x8899aabbccddeeff), Limb(0x0011223344556677)]
    );
}

#[test]
#[cfg(target_pointer_width = "32")]
fn from_be_hex() {
    let n = UintEx::from_be_hex("0011223344556677");
    assert_eq!(n.as_limbs(), &[Limb(0x44556677), Limb(0x00112233)]);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn from_be_hex() {
    let n = UintEx::from_be_hex("00112233445566778899aabbccddeeff");
    assert_eq!(
        n.as_limbs(),
        &[Limb(0x8899aabbccddeeff), Limb(0x0011223344556677)]
    );
}

#[test]
#[cfg(target_pointer_width = "32")]
fn from_le_hex() {
    let n = UintEx::from_le_hex("7766554433221100");
    assert_eq!(n.as_limbs(), &[Limb(0x44556677), Limb(0x00112233)]);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn from_le_hex() {
    let n = UintEx::from_le_hex("ffeeddccbbaa99887766554433221100");
    assert_eq!(
        n.as_limbs(),
        &[Limb(0x8899aabbccddeeff), Limb(0x0011223344556677)]
    );
}

#[cfg(feature = "alloc")]
#[test]
fn hex_upper() {
    let hex = "AAAAAAAABBBBBBBBCCCCCCCCDDDDDDDD";
    let n = U128::from_be_hex(hex);
    assert_eq!(hex, format!("{:X}", n));
}

#[cfg(feature = "alloc")]
#[test]
fn hex_lower() {
    let hex = "aaaaaaaabbbbbbbbccccccccdddddddd";
    let n = U128::from_be_hex(hex);
    assert_eq!(hex, format!("{:x}", n));
}
