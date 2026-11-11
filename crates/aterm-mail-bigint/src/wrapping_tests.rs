// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{U64, Wrapping};

#[test]
fn serde() {
    const TEST: Wrapping<U64> = Wrapping(U64::from_u64(0x0011223344556677));

    let serialized = bincode::serialize(&TEST).unwrap();
    let deserialized: Wrapping<U64> = bincode::deserialize(&serialized).unwrap();

    assert_eq!(TEST, deserialized);
}

#[test]
fn serde_owned() {
    const TEST: Wrapping<U64> = Wrapping(U64::from_u64(0x0011223344556677));

    let serialized = bincode::serialize(&TEST).unwrap();
    let deserialized: Wrapping<U64> = bincode::deserialize_from(serialized.as_slice()).unwrap();

    assert_eq!(TEST, deserialized);
}
