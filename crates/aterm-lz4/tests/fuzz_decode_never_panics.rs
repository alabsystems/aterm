// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Decode-logic robustness: given a plausibly-sized output budget, a corrupt
// or truncated body returns `Ok`/`Err`, never panics or reads out of bounds.
//
// `decompress_size_prepended` trusts its 4-byte size prefix and allocates
// that much up front, so an unbounded random prefix tests allocation (up to
// 4 GiB per call), not decoding. Untrusted scrollback goes through
// aterm-scrollback's bounded wrapper (`MAX_DECOMPRESSED_SIZE`,
// `MAX_COMPRESSION_RATIO`), fuzzed there.

use aterm_lz4::block::{decompress, decompress_into, decompress_size_prepended};

/// Largest declared output size: far above what these inputs decode to, cheap
/// to allocate.
const MAX_DECLARED_SIZE: u32 = 256 * 1024;

/// Deterministic LCG fuzz of every decode entry point with a bounded output
/// budget.
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

        // 1) size-prepended path: [bounded declared size LE][corrupt body].
        let declared = next() % MAX_DECLARED_SIZE;
        let mut framed = Vec::with_capacity(4 + body.len());
        framed.extend_from_slice(&declared.to_le_bytes());
        framed.extend_from_slice(&body);
        let _ = decompress_size_prepended(&framed);

        // 2) explicit min-size path with a bounded, sometimes-tiny budget.
        let min = (next() % 256) as usize;
        let _ = decompress(&body, min);

        // 3) fixed-output-buffer path with a randomly-sized destination.
        let out_len = (next() as usize) % scratch.len();
        let _ = decompress_into(&body, &mut scratch[..out_len]);
    }
}

/// Fuzz biased to valid-looking sequences (token, literals, offset) so the
/// match-copy path sees wild offsets and over-long matches and must return
/// `Err`.
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
        // token: high nibble = literal length, low nibble = match length
        input.push((next() & 0xFF) as u8);
        let lits = (next() % 12) as usize;
        input.extend((0..lits).map(|_| (next() & 0xFF) as u8));
        // a (possibly wild) 2-byte little-endian back-reference offset
        input.push((next() & 0xFF) as u8);
        input.push((next() & 0xFF) as u8);
        // a trailing tail of random extension bytes
        let tail = (next() % 8) as usize;
        input.extend((0..tail).map(|_| (next() & 0xFF) as u8));

        let out_len = (next() as usize) % scratch.len();
        let _ = decompress_into(&input, &mut scratch[..out_len]);
    }
}

/// Well-formed size-prepended frames round-trip losslessly.
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
