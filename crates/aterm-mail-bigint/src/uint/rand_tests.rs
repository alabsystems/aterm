// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{NonZero, RandomMod, U256};
use rand_core::SeedableRng;

#[test]
fn random_mod() {
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(1);

    // Ensure `random_mod` runs in a reasonable amount of time
    let modulus = NonZero::new(U256::from(42u8)).unwrap();
    let res = U256::random_mod(&mut rng, &modulus);

    // Check that the value is in range
    assert!(res >= U256::ZERO);
    assert!(res < U256::from(42u8));

    // Ensure `random_mod` runs in a reasonable amount of time
    // when the modulus is larger than 1 limb
    let modulus = NonZero::new(U256::from(0x10000000000000001u128)).unwrap();
    let res = U256::random_mod(&mut rng, &modulus);

    // Check that the value is in range
    assert!(res >= U256::ZERO);
    assert!(res < U256::from(0x10000000000000001u128));
}
