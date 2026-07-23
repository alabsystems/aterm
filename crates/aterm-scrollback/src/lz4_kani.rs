// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for the inline LZ4 decompressor (#7934).
//!
//! Covered harnesses:
//!   - `lz4_decompress_rejects_undersized_header` — any input shorter than 4
//!     bytes returns `Err(InputTooShort)` rather than panicking.
//!   - `lz4_decompress_no_oob_on_malformed_input` — `decompress_size_prepended`
//!     never panics and never returns output larger than the claimed size
//!     (on bounded adversarial inputs).
//!
//! These prove that a malicious peer feeding garbage into the warm-tier
//! decompression path cannot panic the process or cause the decoder to
//! emit more bytes than the prefix claimed.

use crate::lz4::{Lz4Error, decompress_size_prepended};

// ── Kani proofs ──────────────────────────────────────────────────────────

/// H4 (#7934): decompression rejects inputs shorter than the 4-byte size
/// prefix without panicking.
#[cfg(kani)]
#[kani::proof]
#[kani::unwind(6)]
fn lz4_decompress_rejects_undersized_header() {
    const MAX_LEN: usize = 4;
    let len: usize = kani::any();
    kani::assume(len < 4); // strictly shorter than the prefix

    let mut buf = [0u8; MAX_LEN];
    for i in 0..MAX_LEN {
        buf[i] = kani::any();
    }
    let slice = &buf[..len];

    let result = decompress_size_prepended(slice);
    kani::assert(
        matches!(result, Err(Lz4Error::InputTooShort)),
        "inputs < 4 bytes must return InputTooShort",
    );
}

/// H5 (#7934): decompression on bounded malformed input never panics, and
/// when it succeeds, the output length equals the size encoded in the
/// prefix. This is the key anti-DoS property: an attacker cannot make
/// the decoder read out of bounds or allocate more than it was told.
#[cfg(kani)]
#[kani::proof]
#[kani::unwind(8)]
fn lz4_decompress_no_oob_on_malformed_input() {
    // 4-byte header + small compressed block. Keep total small enough that
    // Kani can enumerate the decode loop.
    const MAX_LEN: usize = 8;
    let len: usize = kani::any();
    kani::assume(len >= 4 && len <= MAX_LEN);

    let mut buf = [0u8; MAX_LEN];
    // Force the prefix to encode a small size so we don't hit the
    // MAX_DECOMPRESSED_SIZE path (which is also verified by returning
    // OutputTooLarge, but that's not the property we care about here).
    buf[0] = kani::any();
    kani::assume(buf[0] <= 16); // original_size <= 16
    buf[1] = 0;
    buf[2] = 0;
    buf[3] = 0;
    for i in 4..MAX_LEN {
        buf[i] = kani::any();
    }
    let slice = &buf[..len];

    let result = decompress_size_prepended(slice);
    let expected_size = buf[0] as usize;

    match result {
        Ok(out) => {
            // Successful decode must match the prefix-claimed size.
            kani::assert(
                out.len() == expected_size,
                "successful decode must match prefix size",
            );
        }
        Err(_) => {
            // Any error is acceptable; the contract is "no panic".
        }
    }
}

// Unit-test equivalents for non-Kani CI coverage.
#[cfg(test)]
mod decode_tests {
    use super::*;

    #[test]
    fn rejects_empty_input() {
        assert!(matches!(
            decompress_size_prepended(&[]),
            Err(Lz4Error::InputTooShort)
        ));
    }

    #[test]
    fn rejects_1_byte_input() {
        assert!(matches!(
            decompress_size_prepended(&[0x00]),
            Err(Lz4Error::InputTooShort)
        ));
    }

    #[test]
    fn rejects_3_byte_input() {
        assert!(matches!(
            decompress_size_prepended(&[0x00, 0x00, 0x00]),
            Err(Lz4Error::InputTooShort)
        ));
    }

    #[test]
    fn zero_size_prefix_empty_body_is_ok() {
        // 4-byte prefix of 0 + 0-byte body = valid "empty" decode.
        let r = decompress_size_prepended(&[0, 0, 0, 0]);
        assert!(r.is_ok(), "empty claim + empty body must succeed: {r:?}");
        assert_eq!(r.unwrap().len(), 0);
    }

    #[test]
    fn garbage_after_valid_prefix_errors_not_panics() {
        // Prefix says "4 bytes", body is garbage. Must not panic.
        for byte in 0u8..=255 {
            let input = [4, 0, 0, 0, byte];
            let _ = decompress_size_prepended(&input);
        }
    }

    #[test]
    fn huge_size_prefix_rejected() {
        // Prefix exceeds MAX_DECOMPRESSED_SIZE (16 MiB).
        let input = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(matches!(
            decompress_size_prepended(&input),
            Err(Lz4Error::OutputTooLarge(_))
        ));
    }
}
