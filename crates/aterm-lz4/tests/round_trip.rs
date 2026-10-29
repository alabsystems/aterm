// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// End-to-end round trips and malformed-input rejection through the public API.

use aterm_lz4::block::decompress_into;
use aterm_lz4::{compress_prepend_size, decompress_size_prepended};

#[test]
fn round_trip_repeated_pattern() {
    let input: Vec<u8> = b"ABCDEFGH".iter().copied().cycle().take(10_000).collect();
    let compressed = compress_prepend_size(&input);
    assert_eq!(decompress_size_prepended(&compressed).unwrap(), input);
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
fn decompress_rejects_too_short_input() {
    assert!(decompress_size_prepended(b"").is_err());
    assert!(decompress_size_prepended(b"abc").is_err());
}

#[test]
fn decompress_rejects_corrupt_payload() {
    let data = [0x04, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF];
    assert!(decompress_size_prepended(&data).is_err());
}

#[test]
fn malformed_literal_length_overruns_output_errors() {
    let mut input = vec![0xE0u8];
    input.extend_from_slice(&[b'x'; 14]);
    input.extend_from_slice(&[0u8, 0u8]);
    let mut output = [0u8; 4];
    decompress_into(&input, &mut output)
        .expect_err("oversized literal length must return a decode Err");
}

#[test]
fn malformed_match_length_overruns_output_errors() {
    let input = vec![0x1Eu8, b'a', 1, 0];
    let mut output = [0u8; 2];
    decompress_into(&input, &mut output)
        .expect_err("oversized match length must return a decode Err");
}
