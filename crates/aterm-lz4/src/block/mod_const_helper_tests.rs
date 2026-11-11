// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

use super::*;

#[test]
fn lz4_block_const_helpers_invariants() {
    assert_eq!(window_size(), 64 * 1024);
    assert_eq!(lz4_min_length(), 13);
    assert_eq!(max_distance(), 65535);
    assert_eq!(min_match(), 4);
    assert!(!is_compressible_block_len(12));
    assert!(is_compressible_block_len(13));
    assert!(valid_match_distance(10, 20));
    assert!(!valid_match_distance(0, 20));
    assert!(!valid_match_distance(25, 20));
    assert!(valid_match_len(4) && !valid_match_len(3));

    let tok = pack_token(15, 7);
    assert_eq!(token_literal_nibble(tok), 15);
    assert_eq!(token_match_nibble(tok), 7);
    assert!(token_needs_literal_ext(tok) && !token_needs_match_ext(tok));

    assert_eq!(checked_max_compressed_size(255), Some(255 + 1 + 16));
    assert_eq!(checked_size_prepended_max_len(255), Some(255 + 1 + 16 + 4));

    let bytes = encode_prepended_size_le(0x1234_5678);
    assert_eq!(decode_prepended_size_le(bytes), 0x1234_5678);

    let mtok = encode_lz4_match_copy_token(12, 5, 0xABCD);
    assert_eq!(decode_lz4_match_copy_token(mtok), (12, 5, 0xABCD));

    let err = DecompressError::OutputTooSmall {
        expected: 64,
        actual: 32,
    };
    assert!(err.is_output_too_small());
    assert_eq!(err.expected_output_size(), Some(64));
}
