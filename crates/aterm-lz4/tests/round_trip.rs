// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Integration tests for the vendored lz4_flex block-mode subset. These
// exercise the public surface through `aterm_lz4::{compress_prepend_size,
// decompress_size_prepended}` (matching the upstream `lz4_flex::block::*`
// entry points) and are what in-tree consumers would call. The internal
// unit tests live alongside the vendored source in `src/block/compress.rs`
// and `src/block/decompress_safe.rs` — these tests cover end-to-end
// roundtrips that are not otherwise exercised at the block-submodule
// level.

use aterm_lz4::block::decompress_into;
use aterm_lz4::{compress_prepend_size, decompress_size_prepended};

#[test]
fn round_trip_empty() {
    let out = compress_prepend_size(b"");
    assert_eq!(decompress_size_prepended(&out).unwrap(), b"");
}

#[test]
fn round_trip_small() {
    let input = b"Hello people, what's up?";
    let out = compress_prepend_size(input);
    assert_eq!(decompress_size_prepended(&out).unwrap(), input);
}

#[test]
fn round_trip_repeated_pattern() {
    // Highly compressible — should shrink significantly.
    let input: Vec<u8> = b"ABCDEFGH".iter().copied().cycle().take(10_000).collect();
    let compressed = compress_prepend_size(&input);
    assert!(
        compressed.len() < input.len() / 4,
        "expected >4x compression on repeated pattern, got {} -> {}",
        input.len(),
        compressed.len()
    );
    assert_eq!(decompress_size_prepended(&compressed).unwrap(), input);
}

#[test]
fn round_trip_all_zeros_large() {
    // 1 MiB of zeros — should compress to a tiny fraction.
    let input = vec![0u8; 1024 * 1024];
    let compressed = compress_prepend_size(&input);
    assert!(
        compressed.len() < input.len() / 50,
        "zeros should compress >50x, got {} -> {}",
        input.len(),
        compressed.len()
    );
    let back = decompress_size_prepended(&compressed).unwrap();
    assert_eq!(back.len(), input.len());
    assert!(back.iter().all(|&b| b == 0));
}

#[test]
fn round_trip_incompressible() {
    // LCG output has no exploitable redundancy at this size.
    let mut input = vec![0u8; 4096];
    let mut state: u32 = 0xDEAD_BEEF;
    for byte in &mut input {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        *byte = (state >> 16) as u8;
    }
    let compressed = compress_prepend_size(&input);
    let back = decompress_size_prepended(&compressed).unwrap();
    assert_eq!(back, input);
}

#[test]
fn round_trip_boundary_sizes() {
    for size in [
        0, 1, 12, 13, 14, 15, 16, 64, 255, 256, 257, 1024, 65_535, 65_536,
    ] {
        let input: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let compressed = compress_prepend_size(&input);
        let back = decompress_size_prepended(&compressed)
            .unwrap_or_else(|e| panic!("decompress failed at size {size}: {e}"));
        assert_eq!(back, input, "round-trip mismatch at size {size}");
    }
}

#[test]
fn size_prefix_is_little_endian_original_length() {
    let input = b"test data for size check";
    let out = compress_prepend_size(input);
    let size = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
    assert_eq!(size as usize, input.len());
}

#[test]
fn deterministic_output_for_same_input() {
    let input: Vec<u8> = b"ABCDEFGH".iter().copied().cycle().take(500).collect();
    let a = compress_prepend_size(&input);
    let b = compress_prepend_size(&input);
    assert_eq!(a, b, "compression must be deterministic");
}

#[test]
fn decompress_rejects_too_short_input() {
    // Need at least 4 bytes for the size prefix.
    assert!(decompress_size_prepended(b"").is_err());
    assert!(decompress_size_prepended(b"abc").is_err());
}

#[test]
fn decompress_rejects_corrupt_payload() {
    // Valid 4-byte size prefix but garbage payload.
    let data = [0x04, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF];
    assert!(decompress_size_prepended(&data).is_err());
}

#[test]
fn round_trip_overlapping_match() {
    // "ABAB..." — LZ4 offset=2, match length far exceeds offset, exercising
    // the overlapping-copy decode path.
    let input: Vec<u8> = b"AB".iter().copied().cycle().take(500).collect();
    let out = compress_prepend_size(&input);
    assert_eq!(decompress_size_prepended(&out).unwrap(), input);
}

// ---------------------------------------------------------------------------
// Malformed-input soundness regressions.
//
// These run against whichever decode path is built (safe-decode by default,
// the unsafe pointer-based decoder under `--no-default-features`). The unsafe
// hot loop copies fixed-width chunks (16-byte literal, 18-byte match) driven
// by attacker-influenced lengths; the per-copy bounds guards in
// `src/block/decompress.rs` must reject overruns with a decode `Err` rather
// than reading/writing out of bounds. Both paths must agree: malformed input
// errors cleanly, never panics or corrupts memory.
// ---------------------------------------------------------------------------

#[test]
fn malformed_literal_length_overruns_output_errors() {
    // token 0xE0 => 14 literal bytes requested, but output is only 4 bytes.
    let mut input = vec![0xE0u8];
    input.extend_from_slice(&[b'x'; 14]);
    input.extend_from_slice(&[0u8, 0u8]);
    let mut output = [0u8; 4];
    decompress_into(&input, &mut output)
        .expect_err("oversized literal length must return a decode Err");
}

#[test]
fn malformed_match_length_overruns_output_errors() {
    // token 0x1E => 1 literal then a 14+MINMATCH match into a 2-byte output.
    let input = vec![0x1Eu8, b'a', 1, 0];
    let mut output = [0u8; 2];
    decompress_into(&input, &mut output)
        .expect_err("oversized match length must return a decode Err");
}

#[test]
fn malformed_large_block_stays_in_bounds() {
    // A bigger output makes the unsafe hot path reachable; feed adversarial
    // literal/offset bytes and require the decoder to stay in bounds (Ok or
    // Err, never panic / OOB).
    let input = vec![
        0x5Au8, b'a', b'b', b'c', b'd', b'e', 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let mut output = [0u8; 64];
    let _ = decompress_into(&input, &mut output);
}

#[test]
fn fuzz_corrupt_input_never_panics() {
    // A daily-driver terminal decompresses scrollback blocks that may be corrupt
    // (truncated/bit-flipped on disk). The block decoder must NEVER panic on
    // arbitrary input — only return Ok or Err. This deterministic fuzz sweeps
    // 50k pseudo-random byte sequences through the raw decoder with a fixed
    // output buffer (no untrusted-size allocation); any panic aborts the test.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    let mut output = vec![0u8; 4096];
    for _ in 0..50_000 {
        let len = (next() % 300) as usize;
        let input: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
        // Reaching the next iteration for every input means none panicked.
        let _ = decompress_into(&input, &mut output);
    }
}
