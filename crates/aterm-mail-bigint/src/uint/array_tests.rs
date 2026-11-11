// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{ArrayDecoding, ArrayEncoding, Limb};
use hex_literal::hex;

#[cfg(target_pointer_width = "32")]
use crate::U64 as UintEx;

#[cfg(target_pointer_width = "64")]
use crate::U128 as UintEx;

/// Byte array that corresponds to `UintEx`
type ByteArray = crate::ByteArray<UintEx>;

#[test]
#[cfg(target_pointer_width = "32")]
fn from_be_byte_array() {
    let n = UintEx::from_be_byte_array(hex!("0011223344556677").into());
    assert_eq!(n.as_limbs(), &[Limb(0x44556677), Limb(0x00112233)]);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn from_be_byte_array() {
    let n = UintEx::from_be_byte_array(hex!("00112233445566778899aabbccddeeff").into());
    assert_eq!(
        n.as_limbs(),
        &[Limb(0x8899aabbccddeeff), Limb(0x0011223344556677)]
    );
}

#[test]
#[cfg(target_pointer_width = "32")]
fn from_le_byte_array() {
    let n = UintEx::from_le_byte_array(hex!("7766554433221100").into());
    assert_eq!(n.as_limbs(), &[Limb(0x44556677), Limb(0x00112233)]);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn from_le_byte_array() {
    let n = UintEx::from_le_byte_array(hex!("ffeeddccbbaa99887766554433221100").into());
    assert_eq!(
        n.as_limbs(),
        &[Limb(0x8899aabbccddeeff), Limb(0x0011223344556677)]
    );
}

#[test]
#[cfg(target_pointer_width = "32")]
fn to_be_byte_array() {
    let expected_bytes = ByteArray::from(hex!("0011223344556677"));
    let actual_bytes = UintEx::from_be_byte_array(expected_bytes).to_be_byte_array();
    assert_eq!(expected_bytes, actual_bytes);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn to_be_byte_array() {
    let expected_bytes = ByteArray::from(hex!("00112233445566778899aabbccddeeff"));
    let actual_bytes = UintEx::from_be_byte_array(expected_bytes).to_be_byte_array();
    assert_eq!(expected_bytes, actual_bytes);
}

#[test]
#[cfg(target_pointer_width = "32")]
fn to_le_byte_array() {
    let expected_bytes = ByteArray::from(hex!("7766554433221100"));
    let actual_bytes = UintEx::from_le_byte_array(expected_bytes).to_le_byte_array();
    assert_eq!(expected_bytes, actual_bytes);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn to_le_byte_array() {
    let expected_bytes = ByteArray::from(hex!("ffeeddccbbaa99887766554433221100"));
    let actual_bytes = UintEx::from_le_byte_array(expected_bytes).to_le_byte_array();
    assert_eq!(expected_bytes, actual_bytes);
}

#[test]
#[cfg(target_pointer_width = "32")]
fn into_uint_be() {
    let expected_bytes = ByteArray::from(hex!("0011223344556677"));
    let actual_bytes = expected_bytes.into_uint_be().to_be_byte_array();
    assert_eq!(expected_bytes, actual_bytes);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn into_uint_be() {
    let expected_bytes = ByteArray::from(hex!("00112233445566778899aabbccddeeff"));
    let actual_bytes = expected_bytes.into_uint_be().to_be_byte_array();
    assert_eq!(expected_bytes, actual_bytes);
}

#[test]
#[cfg(target_pointer_width = "32")]
fn into_uint_le() {
    let expected_bytes = ByteArray::from(hex!("7766554433221100"));
    let actual_bytes = expected_bytes.into_uint_le().to_le_byte_array();
    assert_eq!(expected_bytes, actual_bytes);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn into_uint_le() {
    let expected_bytes = ByteArray::from(hex!("ffeeddccbbaa99887766554433221100"));
    let actual_bytes = expected_bytes.into_uint_le().to_le_byte_array();
    assert_eq!(expected_bytes, actual_bytes);
}
