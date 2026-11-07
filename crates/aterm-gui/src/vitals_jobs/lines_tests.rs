// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::lines::Lines;

#[path = "lines/reference.rs"]
mod reference;

fn check(bytes: &[u8]) {
    let got = Lines::of(bytes);
    assert_eq!(
        [got.ends, got.ended, got.tail, got.drawn],
        reference::classify(bytes),
        "{bytes:?}"
    );
}

#[test]
fn raw_chunks_match_original_including_malformed_csi() {
    const ALPHABET: &[u8] = b"\r\n\0\x1b[0Hmf\xff";
    for len in 0..=5 {
        for mut value in 0..ALPHABET.len().pow(len) {
            let mut bytes = [0; 5];
            for b in &mut bytes[..len as usize] {
                *b = ALPHABET[value % ALPHABET.len()];
                value /= ALPHABET.len();
            }
            check(&bytes[..len as usize]);
        }
    }
    let mut state = 0x519a_c624_d73b_e808_u64;
    let mut bytes = [0; 8192];
    for len in [7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 4096, 8192] {
        for _ in 0..32 {
            for b in &mut bytes[..len] {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *b = state as u8;
            }
            check(&bytes[..len]);
        }
    }
    for final_byte in b"HfdrLMST" {
        let bytes = [b'\r', 0x1b, b'[', b'1', b';', b'2', *final_byte, b'\n'];
        assert!(Lines::of(&bytes).drawn);
        for split in 0..=bytes.len() {
            check(&bytes[..split]);
            check(&bytes[split..]);
        }
    }
}
