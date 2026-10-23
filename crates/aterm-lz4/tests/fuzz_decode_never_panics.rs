// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_lz4::block::{decompress, decompress_into, decompress_size_prepended};

const MAX_DECLARED_SIZE: u32 = 256 * 1024;

#[test]
fn fuzz_safe_decode_never_panics() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };

    let mut scratch = vec![0u8; 4096];
    for _ in 0..120_000u32 {
        let body_len = (next() % 96) as usize;
        let body: Vec<u8> = (0..body_len).map(|_| (next() & 0xFF) as u8).collect();

        let declared = next() % MAX_DECLARED_SIZE;
        let mut framed = Vec::with_capacity(4 + body.len());
        framed.extend_from_slice(&declared.to_le_bytes());
        framed.extend_from_slice(&body);
        let _ = decompress_size_prepended(&framed);

        let min = (next() % 256) as usize;
        let _ = decompress(&body, min);

        let out_len = (next() as usize) % scratch.len();
        let _ = decompress_into(&body, &mut scratch[..out_len]);
    }
}

#[test]
fn fuzz_decode_match_copy_shapes_never_panic() {
    let mut state: u64 = 0xD1B5_4A32_D192_ED03;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    let mut scratch = vec![0u8; 8192];
    for _ in 0..90_000u32 {
        let mut input: Vec<u8> = Vec::with_capacity(32);
        input.push((next() & 0xFF) as u8);
        let lits = (next() % 12) as usize;
        input.extend((0..lits).map(|_| (next() & 0xFF) as u8));
        input.push((next() & 0xFF) as u8);
        input.push((next() & 0xFF) as u8);
        let tail = (next() % 8) as usize;
        input.extend((0..tail).map(|_| (next() & 0xFF) as u8));

        let out_len = (next() as usize) % scratch.len();
        let _ = decompress_into(&input, &mut scratch[..out_len]);
    }
}

#[test]
fn size_prepended_round_trip_is_lossless() {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 24) as u32
    };
    for _ in 0..2_000u32 {
        let len = (next() % 4096) as usize;
        let original: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
        let framed = aterm_lz4::block::compress_prepend_size(&original);
        let decoded = decompress_size_prepended(&framed).expect("valid frame decodes");
        assert_eq!(decoded, original, "round-trip must be lossless");
    }
}
