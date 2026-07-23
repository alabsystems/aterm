// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Robustness fuzz for the SAFE LZ4 block decoder's DECODE LOGIC.
//
// Layering note (important — this test was previously wrong about it):
// `aterm_lz4::block::decompress_size_prepended` is a LOW-LEVEL API whose 4-byte
// little-endian size prefix is *trusted by contract* — it is the value written
// by `compress_prepend_size`, and the decoder eagerly allocates a buffer of that
// declared size (`vec![0; declared]`). Feeding it an unbounded random prefix
// therefore asks it to allocate up to ~4 GiB per call: that is the documented
// cost of the trusted-size contract, NOT a decoder bug. (An earlier version of
// this test fed unbounded prefixes here and "found" that non-bug by hanging for
// hours on multi-gigabyte allocations.)
//
// The genuine untrusted-input attack surface — cold-tier scrollback blocks read
// back from a possibly-corrupt or hostile file — goes through
// `aterm_scrollback`'s bounded wrapper, which rejects an oversized or
// bomb-ratio declared size *before* allocating (see aterm-scrollback/src/lz4.rs,
// `MAX_DECOMPRESSED_SIZE` / `MAX_COMPRESSION_RATIO`); that surface is fuzzed in
// aterm-scrollback's own tests.
//
// What THIS test pins is the decoder's DECODE-LOGIC robustness: given a
// plausibly-sized output budget (as every real caller supplies), a corrupt or
// truncated compressed body must only ever return `Ok`/`Err` — never panic, and
// never read out of bounds — because a panic in the decode path is a
// denial-of-service for a daily-driver terminal.

use aterm_lz4::block::{decompress, decompress_into, decompress_size_prepended};

/// Largest declared output size this fuzz will ask the low-level decoder to
/// budget for. Bounded so the test exercises decode logic, not the trusted-size
/// contract's eager allocation. 256 KiB is far above any token shape these
/// inputs produce yet trivially cheap to allocate.
const MAX_DECLARED_SIZE: u32 = 256 * 1024;

/// Deterministic LCG fuzz: arbitrary corrupt bodies driven through every safe
/// decode entry point with a BOUNDED output budget. The size-prepended path's
/// 4-byte length prefix is masked into a sane range (mirroring what the bounded
/// scrollback wrapper guarantees before this code is ever reached). Any panic
/// aborts the test.
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

        // 1) size-prepended path: build a [declared_size_le][corrupt body] frame
        //    with the declared size bounded to a sane budget (the trusted-size
        //    contract means an unbounded prefix is an allocation request, not a
        //    decode test). This fuzzes prefix parsing + decode on a corrupt body.
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

/// Biased fuzz toward VALID-looking LZ4 token shapes (a literal-length token, a
/// few literals, then a back-reference offset) so the decoder is driven deep into
/// the match-copy path where an out-of-bounds offset or over-long match length
/// must be REJECTED (`Err`), never panic or read out of bounds. Uses the
/// fixed-output-buffer entry point, whose budget is the scratch slice — already
/// bounded, so no trusted-size contract is in play here.
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

/// The trusted-size contract is the LOW-LEVEL API's documented behavior, but a
/// *bounded* declared size must still decode correctly: a round-trip through
/// `compress_prepend_size` / `decompress_size_prepended` returns the original
/// bytes. This pins that the bounding done above does not mask a real decode
/// regression on well-formed frames.
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
