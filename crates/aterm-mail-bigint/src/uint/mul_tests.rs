// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{CheckedMul, U64, U128, U192, U256, Zero};

#[test]
fn mul_wide_zero_and_one() {
    assert_eq!(U64::ZERO.mul_wide(&U64::ZERO), (U64::ZERO, U64::ZERO));
    assert_eq!(U64::ZERO.mul_wide(&U64::ONE), (U64::ZERO, U64::ZERO));
    assert_eq!(U64::ONE.mul_wide(&U64::ZERO), (U64::ZERO, U64::ZERO));
    assert_eq!(U64::ONE.mul_wide(&U64::ONE), (U64::ONE, U64::ZERO));
}

#[test]
fn mul_wide_lo_only() {
    let primes: &[u32] = &[3, 5, 17, 257, 65537];

    for &a_int in primes {
        for &b_int in primes {
            let (lo, hi) = U64::from_u32(a_int).mul_wide(&U64::from_u32(b_int));
            let expected = U64::from_u64(a_int as u64 * b_int as u64);
            assert_eq!(lo, expected);
            assert!(bool::from(hi.is_zero()));
        }
    }
}

#[test]
fn mul_concat_even() {
    assert_eq!(U64::ZERO * U64::MAX, U128::ZERO);
    assert_eq!(U64::MAX * U64::ZERO, U128::ZERO);
    assert_eq!(
        U64::MAX * U64::MAX,
        U128::from_u128(0xfffffffffffffffe_0000000000000001)
    );
    assert_eq!(
        U64::ONE * U64::MAX,
        U128::from_u128(0x0000000000000000_ffffffffffffffff)
    );
}

#[test]
fn mul_concat_mixed() {
    let a = U64::from_u64(0x0011223344556677);
    let b = U128::from_u128(0x8899aabbccddeeff_8899aabbccddeeff);
    assert_eq!(a * b, U192::from(&a).saturating_mul(&b));
    assert_eq!(b * a, U192::from(&b).saturating_mul(&a));
}

#[test]
fn checked_mul_ok() {
    let n = U64::from_u32(0xffff_ffff);
    assert_eq!(
        n.checked_mul(&n).unwrap(),
        U64::from_u64(0xffff_fffe_0000_0001)
    );
}

#[test]
fn checked_mul_overflow() {
    let n = U64::from_u64(0xffff_ffff_ffff_ffff);
    assert!(bool::from(n.checked_mul(&n).is_none()));
}

#[test]
fn saturating_mul_no_overflow() {
    let n = U64::from_u8(8);
    assert_eq!(n.saturating_mul(&n), U64::from_u8(64));
}

#[test]
fn saturating_mul_overflow() {
    let a = U64::from(0xffff_ffff_ffff_ffffu64);
    let b = U64::from(2u8);
    assert_eq!(a.saturating_mul(&b), U64::MAX);
}

#[test]
fn square() {
    let n = U64::from_u64(0xffff_ffff_ffff_ffff);
    let (hi, lo) = n.square().split();
    assert_eq!(lo, U64::from_u64(1));
    assert_eq!(hi, U64::from_u64(0xffff_ffff_ffff_fffe));
}

#[test]
fn square_larger() {
    let n = U256::MAX;
    let (hi, lo) = n.square().split();
    assert_eq!(lo, U256::ONE);
    assert_eq!(hi, U256::MAX.wrapping_sub(&U256::ONE));
}
