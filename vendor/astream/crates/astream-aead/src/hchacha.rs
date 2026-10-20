// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HChaCha20 (draft-irtf-cfrg-xchacha-03 §2.2): the one piece of XChaCha20-Poly1305
//! this crate computes itself.
//!
//! XChaCha20-Poly1305 is ChaCha20-Poly1305 (RFC 8439) under a per-message subkey:
//! HChaCha20 of the key and the first 16 nonce bytes, with the last 8 nonce bytes
//! behind 4 zero bytes as the 96-bit nonce (§2.3). ring seals and opens the
//! ChaCha20-Poly1305 part, so the cipher and the MAC stay ring's; what is computed here
//! is the subkey alone, and only from the key. HChaCha20 is the ChaCha20 block
//! function (RFC 8439 §2.3) without the final addition, returning state words 0-3 and
//! 12-15 instead of a keystream block. It is additions, rotations and XORs on 32-bit
//! words, with no branch or table index that depends on the key or the nonce, so it
//! runs in the same time for every input.

use crate::KEY_LEN;
use zeroize::{Zeroize, Zeroizing};

/// The HChaCha20 input: the first 16 bytes of an XChaCha20 nonce.
pub(crate) const HCHACHA_INPUT_LEN: usize = 16;

/// "expand 32-byte k", RFC 8439 §2.3.
const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

/// The ChaCha quarter round, RFC 8439 §2.1.
#[inline(always)]
const fn quarter_round(a: u32, b: u32, c: u32, d: u32) -> (u32, u32, u32, u32) {
    let a = a.wrapping_add(b);
    let d = (d ^ a).rotate_left(16);
    let c = c.wrapping_add(d);
    let b = (b ^ c).rotate_left(12);
    let a = a.wrapping_add(b);
    let d = (d ^ a).rotate_left(8);
    let c = c.wrapping_add(d);
    let b = (b ^ c).rotate_left(7);
    (a, b, c, d)
}

#[inline(always)]
const fn load_u32_le(b0: u8, b1: u8, b2: u8, b3: u8) -> u32 {
    (b0 as u32) | ((b1 as u32) << 8) | ((b2 as u32) << 16) | ((b3 as u32) << 24)
}

/// One column round and one diagonal round, RFC 8439 §2.3.
#[inline(always)]
const fn double_round(s: &mut [u32; 16]) {
    (s[0], s[4], s[8], s[12]) = quarter_round(s[0], s[4], s[8], s[12]);
    (s[1], s[5], s[9], s[13]) = quarter_round(s[1], s[5], s[9], s[13]);
    (s[2], s[6], s[10], s[14]) = quarter_round(s[2], s[6], s[10], s[14]);
    (s[3], s[7], s[11], s[15]) = quarter_round(s[3], s[7], s[11], s[15]);
    (s[0], s[5], s[10], s[15]) = quarter_round(s[0], s[5], s[10], s[15]);
    (s[1], s[6], s[11], s[12]) = quarter_round(s[1], s[6], s[11], s[12]);
    (s[2], s[7], s[8], s[13]) = quarter_round(s[2], s[7], s[8], s[13]);
    (s[3], s[4], s[9], s[14]) = quarter_round(s[3], s[4], s[9], s[14]);
}

/// HChaCha20 of `key` and `input`: the XChaCha20 subkey for a nonce whose first 16
/// bytes are `input`. The working state, which holds the key, is wiped before this
/// returns, and the subkey wipes itself when dropped.
pub(crate) fn hchacha20(
    key: &[u8; KEY_LEN],
    input: &[u8; HCHACHA_INPUT_LEN],
) -> Zeroizing<[u8; KEY_LEN]> {
    // Words 0-3 the constants, 4-11 the key, 12-15 the input, each little-endian.
    let mut s = [
        SIGMA[0],
        SIGMA[1],
        SIGMA[2],
        SIGMA[3],
        load_u32_le(key[0], key[1], key[2], key[3]),
        load_u32_le(key[4], key[5], key[6], key[7]),
        load_u32_le(key[8], key[9], key[10], key[11]),
        load_u32_le(key[12], key[13], key[14], key[15]),
        load_u32_le(key[16], key[17], key[18], key[19]),
        load_u32_le(key[20], key[21], key[22], key[23]),
        load_u32_le(key[24], key[25], key[26], key[27]),
        load_u32_le(key[28], key[29], key[30], key[31]),
        load_u32_le(input[0], input[1], input[2], input[3]),
        load_u32_le(input[4], input[5], input[6], input[7]),
        load_u32_le(input[8], input[9], input[10], input[11]),
        load_u32_le(input[12], input[13], input[14], input[15]),
    ];
    let mut round = 0usize;
    while round < 10 {
        double_round(&mut s);
        round += 1;
    }
    let mut words = [s[0], s[1], s[2], s[3], s[12], s[13], s[14], s[15]];
    let mut raw = [0u8; KEY_LEN];
    let mut idx = 0usize;
    while idx < 8 {
        let w = words[idx];
        let base = idx * 4;
        raw[base] = (w & 0xFF) as u8;
        raw[base + 1] = ((w >> 8) & 0xFF) as u8;
        raw[base + 2] = ((w >> 16) & 0xFF) as u8;
        raw[base + 3] = ((w >> 24) & 0xFF) as u8;
        idx += 1;
    }
    let subkey = Zeroizing::new(raw);
    raw.zeroize();
    s.zeroize();
    words.zeroize();
    subkey
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDED: &str = include_str!("../tests/fixtures/hchacha20-oracle.tsv");

    fn unhex<const N: usize>(s: &str) -> [u8; N] {
        let v: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        v.try_into().unwrap()
    }

    #[test]
    fn matches_the_draft_irtf_cfrg_xchacha_2_2_1_vector() {
        // draft-irtf-cfrg-xchacha-03 §2.2.1: key 00..1f, input
        // 000000090000004a0000000031415927.
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let input = unhex::<16>("000000090000004a0000000031415927");
        assert_eq!(
            *hchacha20(&key, &input),
            unhex::<32>("82413b4227b27bfed30e42508a877d73a0f9e4d58a74a853c12ec41326d3ecdc")
        );
    }

    #[test]
    fn matches_every_subkey_recorded_from_rustcrypto() {
        // chacha20 0.9.1's `hchacha::<U10>` over four keys and four nonces' first 16
        // bytes, and the draft's vector (astream 7066fab, tests/fixtures).
        let mut rows = 0;
        for line in RECORDED.lines().filter(|l| !l.starts_with('#')) {
            let c: Vec<&str> = line.split('\t').collect();
            let (key, input, subkey) = (unhex::<32>(c[0]), unhex::<16>(c[1]), unhex::<32>(c[2]));
            assert_eq!(*hchacha20(&key, &input), subkey, "{line}");
            rows += 1;
        }
        assert_eq!(rows, 17);
    }

    #[test]
    fn every_input_bit_reaches_the_subkey() {
        // A dropped word or a lost round would leave some input bit without effect.
        let key = [0x5au8; 32];
        let input = [0xa5u8; 16];
        let base = hchacha20(&key, &input);
        for bit in 0..256 {
            let mut k = key;
            k[bit / 8] ^= 1 << (bit % 8);
            assert_ne!(*hchacha20(&k, &input), *base, "key bit {bit}");
        }
        for bit in 0..128 {
            let mut i = input;
            i[bit / 8] ^= 1 << (bit % 8);
            assert_ne!(*hchacha20(&key, &i), *base, "input bit {bit}");
        }
    }
}
