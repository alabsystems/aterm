// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{CheckedMul, Limb, WideWord};

#[test]
fn mul_wide_zero_and_one() {
    assert_eq!(Limb::ZERO.mul_wide(Limb::ZERO), 0);
    assert_eq!(Limb::ZERO.mul_wide(Limb::ONE), 0);
    assert_eq!(Limb::ONE.mul_wide(Limb::ZERO), 0);
    assert_eq!(Limb::ONE.mul_wide(Limb::ONE), 1);
}

#[test]
fn mul_wide() {
    let primes: &[u32] = &[3, 5, 17, 257, 65537];

    for &a_int in primes {
        for &b_int in primes {
            let actual = Limb::from_u32(a_int).mul_wide(Limb::from_u32(b_int));
            let expected = a_int as WideWord * b_int as WideWord;
            assert_eq!(actual, expected);
        }
    }
}

#[test]
#[cfg(target_pointer_width = "32")]
fn checked_mul_ok() {
    let n = Limb::from_u16(0xffff);
    assert_eq!(n.checked_mul(n).unwrap(), Limb::from_u32(0xfffe_0001));
}

#[test]
#[cfg(target_pointer_width = "64")]
fn checked_mul_ok() {
    let n = Limb::from_u32(0xffff_ffff);
    assert_eq!(
        n.checked_mul(n).unwrap(),
        Limb::from_u64(0xffff_fffe_0000_0001)
    );
}

#[test]
fn checked_mul_overflow() {
    let n = Limb::MAX;
    assert!(bool::from(n.checked_mul(n).is_none()));
}
