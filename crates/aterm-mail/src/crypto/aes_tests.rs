// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
#[test]
fn fips_aes_and_rfc3394() {
    let key = Key::new(&hex("000102030405060708090a0b0c0d0e0f")).unwrap();
    let plain: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
    let cipher: [u8; 16] = hex("69c4e0d86a7b0430d8cdb78070b4c55a").try_into().unwrap();
    assert_eq!(key.block(plain, false), cipher);
    assert_eq!(key.block(cipher, true), plain);
    let wrap = hex("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
    assert_eq!(
        &*unwrap(&hex("000102030405060708090a0b0c0d0e0f"), &wrap).unwrap(),
        &hex("00112233445566778899aabbccddeeff")
    );
    let mut damaged = wrap;
    damaged[0] ^= 1;
    assert!(unwrap(&hex("000102030405060708090a0b0c0d0e0f"), &damaged).is_err());
}
