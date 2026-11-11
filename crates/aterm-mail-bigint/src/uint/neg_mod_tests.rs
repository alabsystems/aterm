// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::U256;

#[test]
fn neg_mod_random() {
    let x = U256::from_be_hex("8d16e171674b4e6d8529edba4593802bf30b8cb161dd30aa8e550d41380007c2");
    let p = U256::from_be_hex("928334a4e4be0843ec225a4c9c61df34bdc7a81513e4b6f76f2bfa3148e2e1b5");

    let actual = x.neg_mod(&p);
    let expected =
        U256::from_be_hex("056c53337d72b9d666f86c9256ce5f08cabc1b63b207864ce0d6ecf010e2d9f3");

    assert_eq!(expected, actual);
}

#[test]
fn neg_mod_zero() {
    let x = U256::from_be_hex("0000000000000000000000000000000000000000000000000000000000000000");
    let p = U256::from_be_hex("928334a4e4be0843ec225a4c9c61df34bdc7a81513e4b6f76f2bfa3148e2e1b5");

    let actual = x.neg_mod(&p);
    let expected =
        U256::from_be_hex("0000000000000000000000000000000000000000000000000000000000000000");

    assert_eq!(expected, actual);
}
