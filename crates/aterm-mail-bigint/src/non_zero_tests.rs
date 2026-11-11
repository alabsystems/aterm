// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{NonZero, U64};
use bincode::ErrorKind;

#[test]
fn serde() {
    let test =
        Option::<NonZero<U64>>::from(NonZero::new(U64::from_u64(0x0011223344556677))).unwrap();

    let serialized = bincode::serialize(&test).unwrap();
    let deserialized: NonZero<U64> = bincode::deserialize(&serialized).unwrap();

    assert_eq!(test, deserialized);

    let serialized = bincode::serialize(&U64::ZERO).unwrap();
    assert!(matches!(
        *bincode::deserialize::<NonZero<U64>>(&serialized).unwrap_err(),
        ErrorKind::Custom(message) if message == "invalid value: zero, expected a non-zero value"
    ));
}

#[test]
fn serde_owned() {
    let test =
        Option::<NonZero<U64>>::from(NonZero::new(U64::from_u64(0x0011223344556677))).unwrap();

    let serialized = bincode::serialize(&test).unwrap();
    let deserialized: NonZero<U64> = bincode::deserialize_from(serialized.as_slice()).unwrap();

    assert_eq!(test, deserialized);

    let serialized = bincode::serialize(&U64::ZERO).unwrap();
    assert!(matches!(
        *bincode::deserialize_from::<_, NonZero<U64>>(serialized.as_slice()).unwrap_err(),
        ErrorKind::Custom(message) if message == "invalid value: zero, expected a non-zero value"
    ));
}
