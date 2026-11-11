// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Limb, U256};

#[cfg(feature = "rand")]
use {
    crate::{CheckedMul, Random, U512},
    rand_chacha::ChaChaRng,
    rand_core::{RngCore, SeedableRng},
};

#[test]
fn edge() {
    assert_eq!(U256::ZERO.sqrt_vartime(), U256::ZERO);
    assert_eq!(U256::ONE.sqrt_vartime(), U256::ONE);
    let mut half = U256::ZERO;
    for i in 0..half.limbs.len() / 2 {
        half.limbs[i] = Limb::MAX;
    }
    assert_eq!(U256::MAX.sqrt_vartime(), half,);
}

#[test]
fn simple() {
    let tests = [
        (4u8, 2u8),
        (9, 3),
        (16, 4),
        (25, 5),
        (36, 6),
        (49, 7),
        (64, 8),
        (81, 9),
        (100, 10),
        (121, 11),
        (144, 12),
        (169, 13),
    ];
    for (a, e) in &tests {
        let l = U256::from(*a);
        let r = U256::from(*e);
        assert_eq!(l.sqrt_vartime(), r);
        assert_eq!(l.checked_sqrt_vartime().is_some().unwrap_u8(), 1u8);
    }
}

#[test]
fn nonsquares() {
    assert_eq!(U256::from(2u8).sqrt_vartime(), U256::from(1u8));
    assert_eq!(
        U256::from(2u8).checked_sqrt_vartime().is_some().unwrap_u8(),
        0
    );
    assert_eq!(U256::from(3u8).sqrt_vartime(), U256::from(1u8));
    assert_eq!(
        U256::from(3u8).checked_sqrt_vartime().is_some().unwrap_u8(),
        0
    );
    assert_eq!(U256::from(5u8).sqrt_vartime(), U256::from(2u8));
    assert_eq!(U256::from(6u8).sqrt_vartime(), U256::from(2u8));
    assert_eq!(U256::from(7u8).sqrt_vartime(), U256::from(2u8));
    assert_eq!(U256::from(8u8).sqrt_vartime(), U256::from(2u8));
    assert_eq!(U256::from(10u8).sqrt_vartime(), U256::from(3u8));
}

#[cfg(feature = "rand")]
#[test]
fn fuzz() {
    let mut rng = ChaChaRng::from_seed([7u8; 32]);
    for _ in 0..50 {
        let t = rng.next_u32() as u64;
        let s = U256::from(t);
        let s2 = s.checked_mul(&s).unwrap();
        assert_eq!(s2.sqrt_vartime(), s);
        assert_eq!(s2.checked_sqrt_vartime().is_some().unwrap_u8(), 1);
    }

    for _ in 0..50 {
        let s = U256::random(&mut rng);
        let mut s2 = U512::ZERO;
        s2.limbs[..s.limbs.len()].copy_from_slice(&s.limbs);
        assert_eq!(s.square().sqrt_vartime(), s2);
    }
}
