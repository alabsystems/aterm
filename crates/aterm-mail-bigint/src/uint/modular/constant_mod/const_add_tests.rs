// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{U256, const_residue, impl_modulus, modular::constant_mod::ResidueParams};

impl_modulus!(
    Modulus,
    U256,
    "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551"
);

#[test]
fn add_overflow() {
    let x = U256::from_be_hex("44acf6b7e36c1342c2c5897204fe09504e1e2efb1a900377dbc4e7a6a133ec56");
    let mut x_mod = const_residue!(x, Modulus);

    let y = U256::from_be_hex("d5777c45019673125ad240f83094d4252d829516fac8601ed01979ec1ec1a251");
    let y_mod = const_residue!(y, Modulus);

    x_mod += &y_mod;

    let expected =
        U256::from_be_hex("1a2472fde50286541d97ca6a3592dd75beb9c9646e40c511b82496cfc3926956");

    assert_eq!(expected, x_mod.retrieve());
}
