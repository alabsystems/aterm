// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Limb, NonZero, Random, RandomMod, Uint};
use rand_core::SeedableRng;

macro_rules! test_mul_mod_special {
    ($size:expr, $test_name:ident) => {
        #[test]
        fn $test_name() {
            let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(1);
            let moduli = [
                NonZero::<Limb>::random(&mut rng),
                NonZero::<Limb>::random(&mut rng),
            ];

            for special in &moduli {
                let p =
                    &NonZero::new(Uint::ZERO.wrapping_sub(&Uint::from_word(special.0))).unwrap();

                let minus_one = p.wrapping_sub(&Uint::ONE);

                let base_cases = [
                    (Uint::ZERO, Uint::ZERO, Uint::ZERO),
                    (Uint::ONE, Uint::ZERO, Uint::ZERO),
                    (Uint::ZERO, Uint::ONE, Uint::ZERO),
                    (Uint::ONE, Uint::ONE, Uint::ONE),
                    (minus_one, minus_one, Uint::ONE),
                    (minus_one, Uint::ONE, minus_one),
                    (Uint::ONE, minus_one, minus_one),
                ];
                for (a, b, c) in &base_cases {
                    let x = a.mul_mod_special(&b, *special.as_ref());
                    assert_eq!(*c, x, "{} * {} mod {} = {} != {}", a, b, p, x, c);
                }

                for _i in 0..100 {
                    let a = Uint::<$size>::random_mod(&mut rng, p);
                    let b = Uint::<$size>::random_mod(&mut rng, p);

                    let c = a.mul_mod_special(&b, *special.as_ref());
                    assert!(c < **p, "not reduced: {} >= {} ", c, p);

                    let expected = {
                        let (lo, hi) = a.mul_wide(&b);
                        let mut prod = Uint::<{ 2 * $size }>::ZERO;
                        prod.limbs[..$size].clone_from_slice(&lo.limbs);
                        prod.limbs[$size..].clone_from_slice(&hi.limbs);
                        let mut modulus = Uint::ZERO;
                        modulus.limbs[..$size].clone_from_slice(&p.as_ref().limbs);
                        let reduced = prod.rem(&NonZero::new(modulus).unwrap());
                        let mut expected = Uint::ZERO;
                        expected.limbs[..].clone_from_slice(&reduced.limbs[..$size]);
                        expected
                    };
                    assert_eq!(c, expected, "incorrect result");
                }
            }
        }
    };
}

test_mul_mod_special!(1, mul_mod_special_1);
test_mul_mod_special!(2, mul_mod_special_2);
test_mul_mod_special!(3, mul_mod_special_3);
test_mul_mod_special!(4, mul_mod_special_4);
test_mul_mod_special!(5, mul_mod_special_5);
test_mul_mod_special!(6, mul_mod_special_6);
test_mul_mod_special!(7, mul_mod_special_7);
test_mul_mod_special!(8, mul_mod_special_8);
test_mul_mod_special!(9, mul_mod_special_9);
test_mul_mod_special!(10, mul_mod_special_10);
test_mul_mod_special!(11, mul_mod_special_11);
test_mul_mod_special!(12, mul_mod_special_12);
