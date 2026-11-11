// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{U128, U256, Uint};

const N: U256 =
    U256::from_be_hex("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141");

const N_2: U256 =
    U256::from_be_hex("7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0");

#[test]
fn shr1() {
    assert_eq!(N >> 1, N_2);
}

#[test]
fn shr_wide_1_1_128() {
    assert_eq!(
        Uint::shr_vartime_wide((U128::ONE, U128::ONE), 128),
        (U128::ONE, U128::ZERO)
    );
}

#[test]
fn shr_wide_0_max_1() {
    assert_eq!(
        Uint::shr_vartime_wide((U128::ZERO, U128::MAX), 1),
        (U128::ONE << 127, U128::MAX >> 1)
    );
}

#[test]
fn shr_wide_max_max_256() {
    assert_eq!(
        Uint::shr_vartime_wide((U128::MAX, U128::MAX), 256),
        (U128::ZERO, U128::ZERO)
    );
}
