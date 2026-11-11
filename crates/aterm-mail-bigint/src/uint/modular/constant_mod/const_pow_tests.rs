// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::traits::MultiExponentiate;
use crate::{U256, const_residue, impl_modulus, modular::constant_mod::ResidueParams};

impl_modulus!(
    Modulus,
    U256,
    "9CC24C5DF431A864188AB905AC751B727C9447A8E99E6366E1AD78A21E8D882B"
);

#[test]
fn test_powmod_small_base() {
    let base = U256::from(105u64);
    let base_mod = const_residue!(base, Modulus);

    let exponent =
        U256::from_be_hex("77117F1273373C26C700D076B3F780074D03339F56DD0EFB60E7F58441FD3685");

    let res = base_mod.pow(&exponent);

    let expected =
        U256::from_be_hex("7B2CD7BDDD96C271E6F232F2F415BB03FE2A90BD6CCCEA5E94F1BFD064993766");
    assert_eq!(res.retrieve(), expected);
}

#[test]
fn test_powmod_small_exponent() {
    let base =
        U256::from_be_hex("3435D18AA8313EBBE4D20002922225B53F75DC4453BB3EEC0378646F79B524A4");
    let base_mod = const_residue!(base, Modulus);

    let exponent = U256::from(105u64);

    let res = base_mod.pow(&exponent);

    let expected =
        U256::from_be_hex("89E2A4E99F649A5AE2C18068148C355CA927B34A3245C938178ED00D6EF218AA");
    assert_eq!(res.retrieve(), expected);
}

#[test]
fn test_powmod() {
    let base =
        U256::from_be_hex("3435D18AA8313EBBE4D20002922225B53F75DC4453BB3EEC0378646F79B524A4");
    let base_mod = const_residue!(base, Modulus);

    let exponent =
        U256::from_be_hex("77117F1273373C26C700D076B3F780074D03339F56DD0EFB60E7F58441FD3685");

    let res = base_mod.pow(&exponent);

    let expected =
        U256::from_be_hex("3681BC0FEA2E5D394EB178155A127B0FD2EF405486D354251C385BDD51B9D421");
    assert_eq!(res.retrieve(), expected);
}

#[test]
fn test_multi_exp_array() {
    let base = U256::from(2u8);
    let base_mod = const_residue!(base, Modulus);

    let exponent = U256::from(33u8);
    let bases_and_exponents = [(base_mod, exponent)];
    let res = crate::modular::constant_mod::Residue::<Modulus, { U256::LIMBS }>::multi_exponentiate(
        &bases_and_exponents,
    );

    let expected =
        U256::from_be_hex("0000000000000000000000000000000000000000000000000000000200000000");

    assert_eq!(res.retrieve(), expected);

    let base2 =
        U256::from_be_hex("3435D18AA8313EBBE4D20002922225B53F75DC4453BB3EEC0378646F79B524A4");
    let base2_mod = const_residue!(base2, Modulus);

    let exponent2 =
        U256::from_be_hex("77117F1273373C26C700D076B3F780074D03339F56DD0EFB60E7F58441FD3685");

    let expected = base_mod.pow(&exponent) * base2_mod.pow(&exponent2);
    let bases_and_exponents = [(base_mod, exponent), (base2_mod, exponent2)];
    let res = crate::modular::constant_mod::Residue::<Modulus, { U256::LIMBS }>::multi_exponentiate(
        &bases_and_exponents,
    );

    assert_eq!(res, expected);
}

#[cfg(feature = "alloc")]
#[test]
fn test_multi_exp_slice() {
    let base = U256::from(2u8);
    let base_mod = const_residue!(base, Modulus);

    let exponent = U256::from(33u8);
    let bases_and_exponents = vec![(base_mod, exponent)];
    let res = crate::modular::constant_mod::Residue::<Modulus, { U256::LIMBS }>::multi_exponentiate(
        bases_and_exponents.as_slice(),
    );

    let expected =
        U256::from_be_hex("0000000000000000000000000000000000000000000000000000000200000000");

    assert_eq!(res.retrieve(), expected);

    let base2 =
        U256::from_be_hex("3435D18AA8313EBBE4D20002922225B53F75DC4453BB3EEC0378646F79B524A4");
    let base2_mod = const_residue!(base2, Modulus);

    let exponent2 =
        U256::from_be_hex("77117F1273373C26C700D076B3F780074D03339F56DD0EFB60E7F58441FD3685");

    let expected = base_mod.pow(&exponent) * base2_mod.pow(&exponent2);
    let bases_and_exponents = vec![(base_mod, exponent), (base2_mod, exponent2)];
    let res = crate::modular::constant_mod::Residue::<Modulus, { U256::LIMBS }>::multi_exponentiate(
        bases_and_exponents.as_slice(),
    );

    assert_eq!(res, expected);
}
