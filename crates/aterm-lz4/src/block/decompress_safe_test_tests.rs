// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

use super::*;

#[test]
fn corrupt_integer_run_errors_without_panic() {
    let mut corrupt = vec![0xF0u8]; // token: literal nibble 15 ⇒ read extra integer
    corrupt.extend(std::iter::repeat_n(0xFFu8, 4096));
    let result = decompress(&corrupt, 64);
    assert!(
        result.is_err(),
        "corrupt 0xFF run must error, got Ok(len {:?})",
        result.map(|v| v.len())
    );

    assert!(
        decompress(&[0xFFu8; 64], 64).is_err(),
        "pure 0xFF garbage must decode to Err, not panic or wrongly Ok"
    );
    let mut bad_match = vec![0x1Fu8, b'a', 1, 0];
    bad_match.extend(std::iter::repeat_n(0xFFu8, 4096));
    assert!(
        decompress(&bad_match, 64).is_err(),
        "corrupt match-length 0xFF run must error without panic"
    );
    let _ = decompress(&[], 0);
}

#[test]
fn offset_oob() {
    // Slow path: offset 2 > pos 1, and offset 0 (invalid in LZ4 block format).
    assert!(matches!(
        decompress(&[0x10, b'a', 2, 0], 8),
        Err(DecompressError::OffsetOutOfBounds)
    ));
    assert!(matches!(
        decompress(&[0x10, b'a', 0, 0], 8),
        Err(DecompressError::OffsetOutOfBounds)
    ));

    // Fast path (`does_token_fit`, >= 18 trailing input bytes, >= 35 free output bytes):
    // both `offset > pos` and `offset == 0` must fail closed instead of saturating to 0.
    let mut fast_oob = vec![0x10, b'a', 5, 0, 0xE0];
    fast_oob.extend_from_slice(&[b'z'; 24]);
    assert!(matches!(
        decompress(&fast_oob, 128),
        Err(DecompressError::OffsetOutOfBounds)
    ));

    let mut fast_zero = vec![0x10, b'a', 0, 0, 0xE0];
    fast_zero.extend_from_slice(&[b'z'; 24]);
    assert!(matches!(
        decompress(&fast_zero, 128),
        Err(DecompressError::OffsetOutOfBounds)
    ));

    // Truncated literal run (4 literals declared, 3 bytes left).
    assert!(matches!(
        decompress(&[0x40, b'a', 1, 0], 4),
        Err(DecompressError::LiteralOutOfBounds)
    ));
}
