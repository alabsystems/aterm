// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{ConcatMixed, U64, U128, U192};

#[test]
fn concat() {
    let hi = U64::from_u64(0x0011223344556677);
    let lo = U64::from_u64(0x8899aabbccddeeff);
    assert_eq!(
        hi.concat(&lo),
        U128::from_be_hex("00112233445566778899aabbccddeeff")
    );
}

#[test]
fn concat_mixed() {
    let a = U64::from_u64(0x0011223344556677);
    let b = U128::from_u128(0x8899aabbccddeeff_8899aabbccddeeff);
    assert_eq!(
        a.concat_mixed(&b),
        U192::from_be_hex("00112233445566778899aabbccddeeff8899aabbccddeeff")
    );
    assert_eq!(
        b.concat_mixed(&a),
        U192::from_be_hex("8899aabbccddeeff8899aabbccddeeff0011223344556677")
    );
}

#[test]
fn convert() {
    let res: U128 = U64::ONE.mul_wide(&U64::ONE).into();
    assert_eq!(res, U128::ONE);

    let res: U128 = U64::ONE.square_wide().into();
    assert_eq!(res, U128::ONE);
}
