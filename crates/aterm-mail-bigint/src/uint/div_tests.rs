// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{Limb, U256, limb::HI_BIT};

#[cfg(feature = "rand")]
use {
    crate::{CheckedMul, Random},
    rand_chacha::ChaChaRng,
    rand_core::RngCore,
    rand_core::SeedableRng,
};

#[test]
fn div_word() {
    for (n, d, e, ee) in &[
        (200u64, 2u64, 100u64, 0),
        (100u64, 25u64, 4u64, 0),
        (100u64, 10u64, 10u64, 0),
        (1024u64, 8u64, 128u64, 0),
        (27u64, 13u64, 2u64, 1u64),
        (26u64, 13u64, 2u64, 0u64),
        (14u64, 13u64, 1u64, 1u64),
        (13u64, 13u64, 1u64, 0u64),
        (12u64, 13u64, 0u64, 12u64),
        (1u64, 13u64, 0u64, 1u64),
    ] {
        let lhs = U256::from(*n);
        let rhs = U256::from(*d);
        let (q, r, is_some) = lhs.ct_div_rem(&rhs);
        assert!(is_some.is_true_vartime());
        assert_eq!(U256::from(*e), q);
        assert_eq!(U256::from(*ee), r);
    }
}

#[cfg(feature = "rand")]
#[test]
fn div() {
    let mut rng = ChaChaRng::from_seed([7u8; 32]);
    for _ in 0..25 {
        let num = U256::random(&mut rng).shr_vartime(128);
        let den = U256::random(&mut rng).shr_vartime(128);
        let n = num.checked_mul(&den);
        if n.is_some().into() {
            let (q, _, is_some) = n.unwrap().ct_div_rem(&den);
            assert!(is_some.is_true_vartime());
            assert_eq!(q, num);
        }
    }
}

#[test]
fn div_max() {
    let mut a = U256::ZERO;
    let mut b = U256::ZERO;
    b.limbs[b.limbs.len() - 1] = Limb(Word::MAX);
    let q = a.wrapping_div(&b);
    assert_eq!(q, Uint::ZERO);
    a.limbs[a.limbs.len() - 1] = Limb(1 << (HI_BIT - 7));
    b.limbs[b.limbs.len() - 1] = Limb(0x82 << (HI_BIT - 7));
    let q = a.wrapping_div(&b);
    assert_eq!(q, Uint::ZERO);
}

#[test]
fn div_zero() {
    let (q, r, is_some) = U256::ONE.ct_div_rem(&U256::ZERO);
    assert!(!is_some.is_true_vartime());
    assert_eq!(q, U256::ZERO);
    assert_eq!(r, U256::ONE);
}

#[test]
fn div_one() {
    let (q, r, is_some) = U256::from(10u8).ct_div_rem(&U256::ONE);
    assert!(is_some.is_true_vartime());
    assert_eq!(q, U256::from(10u8));
    assert_eq!(r, U256::ZERO);
}

#[test]
fn reduce_one() {
    let (r, is_some) = U256::from(10u8).const_rem(&U256::ONE);
    assert!(is_some.is_true_vartime());
    assert_eq!(r, U256::ZERO);
}

#[test]
fn reduce_zero() {
    let u = U256::from(10u8);
    let (r, is_some) = u.const_rem(&U256::ZERO);
    assert!(!is_some.is_true_vartime());
    assert_eq!(r, u);
}

#[test]
fn reduce_tests() {
    let (r, is_some) = U256::from(10u8).const_rem(&U256::from(2u8));
    assert!(is_some.is_true_vartime());
    assert_eq!(r, U256::ZERO);
    let (r, is_some) = U256::from(10u8).const_rem(&U256::from(3u8));
    assert!(is_some.is_true_vartime());
    assert_eq!(r, U256::ONE);
    let (r, is_some) = U256::from(10u8).const_rem(&U256::from(7u8));
    assert!(is_some.is_true_vartime());
    assert_eq!(r, U256::from(3u8));
}

#[test]
fn reduce_tests_wide_zero_padded() {
    let (r, is_some) = U256::const_rem_wide((U256::from(10u8), U256::ZERO), &U256::from(2u8));
    assert!(is_some.is_true_vartime());
    assert_eq!(r, U256::ZERO);
    let (r, is_some) = U256::const_rem_wide((U256::from(10u8), U256::ZERO), &U256::from(3u8));
    assert!(is_some.is_true_vartime());
    assert_eq!(r, U256::ONE);
    let (r, is_some) = U256::const_rem_wide((U256::from(10u8), U256::ZERO), &U256::from(7u8));
    assert!(is_some.is_true_vartime());
    assert_eq!(r, U256::from(3u8));
}

#[test]
fn reduce_max() {
    let mut a = U256::ZERO;
    let mut b = U256::ZERO;
    b.limbs[b.limbs.len() - 1] = Limb(Word::MAX);
    let r = a.wrapping_rem(&b);
    assert_eq!(r, Uint::ZERO);
    a.limbs[a.limbs.len() - 1] = Limb(1 << (HI_BIT - 7));
    b.limbs[b.limbs.len() - 1] = Limb(0x82 << (HI_BIT - 7));
    let r = a.wrapping_rem(&b);
    assert_eq!(r, a);
}

#[cfg(feature = "rand")]
#[test]
fn rem2krand() {
    let mut rng = ChaChaRng::from_seed([7u8; 32]);
    for _ in 0..25 {
        let num = U256::random(&mut rng);
        let k = (rng.next_u32() % 256) as usize;
        let den = U256::ONE.shl_vartime(k);

        let a = num.rem2k(k);
        let e = num.wrapping_rem(&den);
        assert_eq!(a, e);
    }
}

#[allow(clippy::op_ref)]
#[test]
fn rem_trait() {
    let a = U256::from(10u64);
    let b = NonZero::new(U256::from(3u64)).unwrap();
    let c = U256::from(1u64);

    assert_eq!(a % b, c);
    assert_eq!(a % &b, c);
    assert_eq!(&a % b, c);
    assert_eq!(&a % &b, c);
}
