// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

//! LZ4 block format (<https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md>).

pub(crate) mod compress;
pub(crate) mod hashtable;

pub(crate) mod decompress_safe;
pub(crate) use decompress_safe as decompress;

pub use compress::*;
pub use decompress::*;

use core::{error::Error, fmt};

pub(crate) const WINDOW_SIZE: usize = 64 * 1024;
const MFLIMIT: usize = 12;
const LAST_LITERALS: usize = 5;
const END_OFFSET: usize = LAST_LITERALS + 1;
const LZ4_MIN_LENGTH: usize = MFLIMIT + 1;
const MAXD_LOG: usize = 16;
const MAX_DISTANCE: usize = (1 << MAXD_LOG) - 1;
const MINMATCH: usize = 4;

/// An error representing invalid compressed data.
#[derive(Debug)]
#[non_exhaustive]
pub enum DecompressError {
    /// The provided output is too small.
    OutputTooSmall {
        /// Minimum expected output size.
        expected: usize,
        /// Actual size of output.
        actual: usize,
    },
    /// Literal is out of bounds of the input.
    LiteralOutOfBounds,
    /// Expected another byte, but none found.
    ExpectedAnotherByte,
    /// Deduplication offset out of bounds (not in buffer).
    OffsetOutOfBounds,
}

#[derive(Debug)]
#[non_exhaustive]
/// Errors that can happen during compression.
pub enum CompressError {
    /// The provided output is too small.
    OutputTooSmall,
}

impl fmt::Display for DecompressError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DecompressError::OutputTooSmall { expected, actual } => {
                f.write_str("provided output is too small for the decompressed data, actual ")?;
                fmt::Display::fmt(actual, f)?;
                f.write_str(", expected ")?;
                fmt::Display::fmt(expected, f)
            }
            DecompressError::LiteralOutOfBounds => {
                f.write_str("literal is out of bounds of the input")
            }
            DecompressError::ExpectedAnotherByte => {
                f.write_str("expected another byte, found none")
            }
            DecompressError::OffsetOutOfBounds => {
                f.write_str("the offset to copy is not contained in the decompressed buffer")
            }
        }
    }
}

impl DecompressError {
    /// Whether this error indicates the destination buffer was smaller than required.
    #[inline]
    #[must_use]
    pub const fn is_output_too_small(&self) -> bool {
        matches!(self, DecompressError::OutputTooSmall { .. })
    }

    /// Return the minimum expected output size when [`Self::OutputTooSmall`].
    #[inline]
    #[must_use]
    pub const fn expected_output_size(&self) -> Option<usize> {
        match *self {
            DecompressError::OutputTooSmall { expected, .. } => Some(expected),
            _ => None,
        }
    }
}

impl fmt::Display for CompressError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CompressError::OutputTooSmall => f.write_str(
                "output is too small for the compressed data, use get_maximum_output_size to \
                 reserve enough space",
            ),
        }
    }
}

impl Error for DecompressError {}

impl Error for CompressError {}

/// Return the LZ4 sliding window size (`64 KiB`).
#[inline]
#[must_use]
pub const fn window_size() -> usize {
    WINDOW_SIZE
}

/// Return the LZ4 minimum compressible block length (`13` bytes).
#[inline]
#[must_use]
pub const fn lz4_min_length() -> usize {
    LZ4_MIN_LENGTH
}

/// Return the maximum backward match distance (`65,535` bytes).
#[inline]
#[must_use]
pub const fn max_distance() -> usize {
    MAX_DISTANCE
}

/// Return the minimum LZ4 duplicate match length (`4` bytes).
#[inline]
#[must_use]
pub const fn min_match() -> usize {
    MINMATCH
}

/// Return `true` iff an independent block of `input_len` bytes is long enough to contain matches.
#[inline]
#[must_use]
pub const fn is_compressible_block_len(input_len: usize) -> bool {
    input_len >= LZ4_MIN_LENGTH
}

/// Return `true` iff `distance` is a valid LZ4 backward offset at `output_pos`.
#[inline]
#[must_use]
pub const fn valid_match_distance(distance: usize, output_pos: usize) -> bool {
    distance > 0 && distance <= MAX_DISTANCE && distance <= output_pos
}

/// Return `true` iff `match_len` meets the LZ4 minimum match length (`>= 4`).
#[inline]
#[must_use]
pub const fn valid_match_len(match_len: usize) -> bool {
    match_len >= MINMATCH
}

/// High nibble of an LZ4 sequence token (literal length base `0..=15`).
#[inline]
#[must_use]
pub const fn token_literal_nibble(token: u8) -> usize {
    (token >> 4) as usize
}

/// Low nibble of an LZ4 sequence token (match length base `0..=15`).
#[inline]
#[must_use]
pub const fn token_match_nibble(token: u8) -> usize {
    (token & 0x0f) as usize
}

/// Pack literal and match nibbles (`0..=15`) into an LZ4 sequence token byte.
#[inline]
#[must_use]
pub const fn pack_token(lit_nibble: u8, match_nibble: u8) -> u8 {
    ((lit_nibble & 0x0f) << 4) | (match_nibble & 0x0f)
}

/// Return `true` iff `token`'s literal nibble is `15` (continuation length bytes follow).
#[inline]
#[must_use]
pub const fn token_needs_literal_ext(token: u8) -> bool {
    (token >> 4) == 0x0f
}

/// Return `true` iff `token`'s match nibble is `15` (continuation length bytes follow).
#[inline]
#[must_use]
pub const fn token_needs_match_ext(token: u8) -> bool {
    (token & 0x0f) == 0x0f
}

/// Checked worst-case compressed size for `input_len` bytes (`input_len + input_len / 255 + 16`).
#[inline]
#[must_use]
pub const fn checked_max_compressed_size(input_len: usize) -> Option<usize> {
    let extra = input_len / 255;
    match input_len.checked_add(extra) {
        Some(n) => n.checked_add(16),
        None => None,
    }
}

/// Checked worst-case compressed size including the 4-byte little-endian uncompressed-size prefix.
#[inline]
#[must_use]
pub const fn checked_size_prepended_max_len(input_len: usize) -> Option<usize> {
    match checked_max_compressed_size(input_len) {
        Some(n) => n.checked_add(4),
        None => None,
    }
}

/// Encode a 32-bit uncompressed length into its 4-byte little-endian prefix.
#[inline]
#[must_use]
pub const fn encode_prepended_size_le(uncompressed_len: u32) -> [u8; 4] {
    [
        uncompressed_len as u8,
        (uncompressed_len >> 8) as u8,
        (uncompressed_len >> 16) as u8,
        (uncompressed_len >> 24) as u8,
    ]
}

/// Decode a 4-byte little-endian uncompressed length prefix.
#[inline]
#[must_use]
pub const fn decode_prepended_size_le(hdr: [u8; 4]) -> u32 {
    (hdr[0] as u32) | ((hdr[1] as u32) << 8) | ((hdr[2] as u32) << 16) | ((hdr[3] as u32) << 24)
}

/// Encode an LZ4 token byte plus its 16-bit little-endian match offset into a 3-byte frame.
#[inline]
#[must_use]
pub const fn encode_lz4_match_copy_token(lit_nibble: u8, match_nibble: u8, offset: u16) -> [u8; 3] {
    [
        pack_token(lit_nibble, match_nibble),
        (offset & 0xFF) as u8,
        ((offset >> 8) & 0xFF) as u8,
    ]
}

/// Decode a 3-byte LZ4 `(token, offset_lo, offset_hi)` header into `(lit_nibble, match_nibble, offset)`.
#[inline]
#[must_use]
pub const fn decode_lz4_match_copy_token(bytes: [u8; 3]) -> (u8, u8, u16) {
    let tok = bytes[0];
    let offset = (bytes[1] as u16) | ((bytes[2] as u16) << 8);
    (tok >> 4, tok & 0x0f, offset)
}

/// Read the 4-byte little-endian uncompressed length prefix and return `(len, remaining_bytes)`.
#[inline]
pub fn uncompressed_size(input: &[u8]) -> Result<(usize, &[u8]), DecompressError> {
    let (b0, b1, b2, b3) = match (input.first(), input.get(1), input.get(2), input.get(3)) {
        (Some(&b0), Some(&b1), Some(&b2), Some(&b3)) => (b0, b1, b2, b3),
        _ => return Err(DecompressError::ExpectedAnotherByte),
    };
    let uncompressed_size = decode_prepended_size_le([b0, b1, b2, b3]) as usize;
    let rest = match input.get(4..) {
        Some(rest) => rest,
        None => return Err(DecompressError::ExpectedAnotherByte),
    };
    Ok((uncompressed_size, rest))
}

#[cfg(test)]
mod const_helper_tests {
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
}
