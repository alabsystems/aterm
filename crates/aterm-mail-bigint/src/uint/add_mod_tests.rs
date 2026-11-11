// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Limb, NonZero, Random, RandomMod, U256, Uint};
use rand_core::SeedableRng;

// TODO(tarcieri): additional tests + proptests

#[test]
fn add_mod_nist_p256() {
    let a = U256::from_be_hex("44acf6b7e36c1342c2c5897204fe09504e1e2efb1a900377dbc4e7a6a133ec56");
    let b = U256::from_be_hex("d5777c45019673125ad240f83094d4252d829516fac8601ed01979ec1ec1a251");
    let n = U256::from_be_hex("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551");

    let actual = a.add_mod(&b, &n);
    let expected =
        U256::from_be_hex("1a2472fde50286541d97ca6a3592dd75beb9c9646e40c511b82496cfc3926956");

    assert_eq!(expected, actual);
}

macro_rules! test_add_mod_special {
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
                    (Uint::ONE, Uint::ZERO, Uint::ONE),
                    (Uint::ZERO, Uint::ONE, Uint::ONE),
                    (minus_one, Uint::ONE, Uint::ZERO),
                    (Uint::ONE, minus_one, Uint::ZERO),
                ];
                for (a, b, c) in &base_cases {
                    let x = a.add_mod_special(b, *special.as_ref());
                    assert_eq!(*c, x, "{} + {} mod {} = {} != {}", a, b, p, x, c);
                }

                for _i in 0..100 {
                    let a = Uint::<$size>::random_mod(&mut rng, p);
                    let b = Uint::<$size>::random_mod(&mut rng, p);

                    let c = a.add_mod_special(&b, *special.as_ref());
                    assert!(c < **p, "not reduced: {} >= {} ", c, p);

                    let expected = a.add_mod(&b, p);
                    assert_eq!(c, expected, "incorrect result");
                }
            }
        }
    };
}

test_add_mod_special!(1, add_mod_special_1);
test_add_mod_special!(2, add_mod_special_2);
test_add_mod_special!(3, add_mod_special_3);
test_add_mod_special!(4, add_mod_special_4);
test_add_mod_special!(5, add_mod_special_5);
test_add_mod_special!(6, add_mod_special_6);
test_add_mod_special!(7, add_mod_special_7);
test_add_mod_special!(8, add_mod_special_8);
test_add_mod_special!(9, add_mod_special_9);
test_add_mod_special!(10, add_mod_special_10);
test_add_mod_special!(11, add_mod_special_11);
test_add_mod_special!(12, add_mod_special_12);
