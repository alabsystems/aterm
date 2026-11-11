// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Limb, U128, U256, Uint};

const N: U256 =
    U256::from_be_hex("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141");

const TWO_N: U256 =
    U256::from_be_hex("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFD755DB9CD5E9140777FA4BD19A06C8282");

const FOUR_N: U256 =
    U256::from_be_hex("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFAEABB739ABD2280EEFF497A3340D90504");

const SIXTY_FIVE: U256 =
    U256::from_be_hex("FFFFFFFFFFFFFFFD755DB9CD5E9140777FA4BD19A06C82820000000000000000");

const EIGHTY_EIGHT: U256 =
    U256::from_be_hex("FFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD03641410000000000000000000000");

const SIXTY_FOUR: U256 =
    U256::from_be_hex("FFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD03641410000000000000000");

#[test]
fn shl_simple() {
    let mut t = U256::from(1u8);
    assert_eq!(t << 1, U256::from(2u8));
    t = U256::from(3u8);
    assert_eq!(t << 8, U256::from(0x300u16));
}

#[test]
fn shl1() {
    assert_eq!(N << 1, TWO_N);
}

#[test]
fn shl2() {
    assert_eq!(N << 2, FOUR_N);
}

#[test]
fn shl65() {
    assert_eq!(N << 65, SIXTY_FIVE);
}

#[test]
fn shl88() {
    assert_eq!(N << 88, EIGHTY_EIGHT);
}

#[test]
fn shl256() {
    assert_eq!(N << 256, U256::default());
}

#[test]
fn shl64() {
    assert_eq!(N << 64, SIXTY_FOUR);
}

#[test]
fn shl_wide_1_1_128() {
    assert_eq!(
        Uint::shl_vartime_wide((U128::ONE, U128::ONE), 128),
        (U128::ZERO, U128::ONE)
    );
}

#[test]
fn shl_wide_max_0_1() {
    assert_eq!(
        Uint::shl_vartime_wide((U128::MAX, U128::ZERO), 1),
        (U128::MAX.sbb(&U128::ONE, Limb::ZERO).0, U128::ONE)
    );
}

#[test]
fn shl_wide_max_max_256() {
    assert_eq!(
        Uint::shl_vartime_wide((U128::MAX, U128::MAX), 256),
        (U128::ZERO, U128::ZERO)
    );
}
