// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Encoding, U128};
use subtle::ConditionallySelectable;

#[cfg(feature = "alloc")]
use alloc::format;

#[cfg(feature = "serde")]
use crate::U64;

#[cfg(feature = "alloc")]
#[test]
fn debug() {
    let hex = "AAAAAAAABBBBBBBBCCCCCCCCDDDDDDDD";
    let n = U128::from_be_hex(hex);

    assert_eq!(
        format!("{:?}", n),
        "Uint(0xAAAAAAAABBBBBBBBCCCCCCCCDDDDDDDD)"
    );
}

#[cfg(feature = "alloc")]
#[test]
fn display() {
    let hex = "AAAAAAAABBBBBBBBCCCCCCCCDDDDDDDD";
    let n = U128::from_be_hex(hex);

    use alloc::string::ToString;
    assert_eq!(hex, n.to_string());

    let hex = "AAAAAAAABBBBBBBB0000000000000000";
    let n = U128::from_be_hex(hex);
    assert_eq!(hex, n.to_string());

    let hex = "AAAAAAAABBBBBBBB00000000DDDDDDDD";
    let n = U128::from_be_hex(hex);
    assert_eq!(hex, n.to_string());

    let hex = "AAAAAAAABBBBBBBB0CCCCCCCDDDDDDDD";
    let n = U128::from_be_hex(hex);
    assert_eq!(hex, n.to_string());
}

#[test]
fn from_bytes() {
    let a = U128::from_be_hex("AAAAAAAABBBBBBBB0CCCCCCCDDDDDDDD");

    let be_bytes = a.to_be_bytes();
    let le_bytes = a.to_le_bytes();
    for i in 0..16 {
        assert_eq!(le_bytes[i], be_bytes[15 - i]);
    }

    let a_from_be = U128::from_be_bytes(be_bytes);
    let a_from_le = U128::from_le_bytes(le_bytes);
    assert_eq!(a_from_be, a_from_le);
    assert_eq!(a_from_be, a);
}

#[test]
fn conditional_select() {
    let a = U128::from_be_hex("00002222444466668888AAAACCCCEEEE");
    let b = U128::from_be_hex("11113333555577779999BBBBDDDDFFFF");

    let select_0 = U128::conditional_select(&a, &b, 0.into());
    assert_eq!(a, select_0);

    let select_1 = U128::conditional_select(&a, &b, 1.into());
    assert_eq!(b, select_1);
}

#[cfg(feature = "serde")]
#[test]
fn serde() {
    const TEST: U64 = U64::from_u64(0x0011223344556677);

    let serialized = bincode::serialize(&TEST).unwrap();
    let deserialized: U64 = bincode::deserialize(&serialized).unwrap();

    assert_eq!(TEST, deserialized);
}

#[cfg(feature = "serde")]
#[test]
fn serde_owned() {
    const TEST: U64 = U64::from_u64(0x0011223344556677);

    let serialized = bincode::serialize(&TEST).unwrap();
    let deserialized: U64 = bincode::deserialize_from(serialized.as_slice()).unwrap();

    assert_eq!(TEST, deserialized);
}
