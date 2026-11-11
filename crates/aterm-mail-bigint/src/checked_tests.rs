// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Checked, U64};
use subtle::{Choice, ConstantTimeEq, CtOption};

#[test]
fn serde() {
    let test = Checked::new(U64::from_u64(0x0011223344556677));

    let serialized = bincode::serialize(&test).unwrap();
    let deserialized: Checked<U64> = bincode::deserialize(&serialized).unwrap();

    assert!(bool::from(test.ct_eq(&deserialized)));

    let test = Checked::new(U64::ZERO) - Checked::new(U64::ONE);
    assert!(bool::from(
        test.ct_eq(&Checked(CtOption::new(U64::ZERO, Choice::from(0))))
    ));

    let serialized = bincode::serialize(&test).unwrap();
    let deserialized: Checked<U64> = bincode::deserialize(&serialized).unwrap();

    assert!(bool::from(test.ct_eq(&deserialized)));
}

#[test]
fn serde_owned() {
    let test = Checked::new(U64::from_u64(0x0011223344556677));

    let serialized = bincode::serialize(&test).unwrap();
    let deserialized: Checked<U64> = bincode::deserialize_from(serialized.as_slice()).unwrap();

    assert!(bool::from(test.ct_eq(&deserialized)));

    let test = Checked::new(U64::ZERO) - Checked::new(U64::ONE);
    assert!(bool::from(
        test.ct_eq(&Checked(CtOption::new(U64::ZERO, Choice::from(0))))
    ));

    let serialized = bincode::serialize(&test).unwrap();
    let deserialized: Checked<U64> = bincode::deserialize_from(serialized.as_slice()).unwrap();

    assert!(bool::from(test.ct_eq(&deserialized)));
}
