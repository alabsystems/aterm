// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Thin wrapper over [`aterm_lz4`] for scrollback compression.

use std::fmt;

/// Error type for LZ4 decompression failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lz4Error {
    /// Input is too short to contain valid data (shorter than the 4-byte size prefix).
    InputTooShort,
    /// The size prefix indicates a length that exceeds the scrollback safety cap
    /// ([`MAX_DECOMPRESSED_SIZE`]).
    OutputTooLarge(usize),
    /// A back-reference offset is zero or exceeds the output written so far.
    InvalidOffset {
        /// The offset value from the compressed stream.
        offset: usize,
        /// How many bytes have been written so far.
        output_pos: usize,
    },
    /// The decompressed output does not match the expected size from the prefix.
    SizeMismatch {
        /// Expected size from the 4-byte prefix.
        expected: usize,
        /// Actual decompressed size.
        actual: usize,
    },
    /// Compressed data ended unexpectedly mid-sequence, or was otherwise rejected by the decoder
    /// for a reason not covered by the other variants.
    UnexpectedEof,
    /// The claimed decompressed size implies a compression ratio over [`MAX_COMPRESSION_RATIO`].
    CompressionBombSuspected {
        /// Claimed decompressed size (from the 4-byte LE prefix).
        claimed_size: usize,
        /// Size of the compressed body (input minus 4-byte prefix).
        compressed_body_len: usize,
    },
}

impl Lz4Error {
    /// Render the `Display` message without `format_args!`, whose expansion embeds an unsafe
    /// `fmt::Arguments` construction the Trust lane (`ship trust run`) fails closed on
    /// (`dec_string` renders decimals like `usize`'s `Display`).
    pub(crate) fn to_message(&self) -> String {
        use crate::error::dec_string;
        match self {
            Self::InputTooShort => String::from("LZ4: input too short"),
            Self::OutputTooLarge(size) => {
                let mut s = String::from("LZ4: decompressed size ");
                s.push_str(&dec_string(*size));
                s.push_str(" exceeds safety limit");
                s
            }
            Self::InvalidOffset { offset, output_pos } => {
                let mut s = String::from("LZ4: invalid back-reference offset ");
                s.push_str(&dec_string(*offset));
                s.push_str(" at output position ");
                s.push_str(&dec_string(*output_pos));
                s
            }
            Self::SizeMismatch { expected, actual } => {
                let mut s = String::from("LZ4: size mismatch: expected ");
                s.push_str(&dec_string(*expected));
                s.push_str(" bytes, got ");
                s.push_str(&dec_string(*actual));
                s
            }
            Self::UnexpectedEof => String::from("LZ4: unexpected end of compressed data"),
            Self::CompressionBombSuspected {
                claimed_size,
                compressed_body_len,
            } => {
                let mut s = String::from("LZ4: claimed size ");
                s.push_str(&dec_string(*claimed_size));
                s.push_str(" from ");
                s.push_str(&dec_string(*compressed_body_len));
                s.push_str("-byte body exceeds safe compression ratio (possible compression bomb)");
                s
            }
        }
    }
}

impl fmt::Display for Lz4Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_message())
    }
}

impl std::error::Error for Lz4Error {}

const MAX_DECOMPRESSED_SIZE: usize = crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES;

const MAX_COMPRESSION_RATIO: usize = 512;

/// Compress `input` using LZ4 block format, prepending the original size as a 4-byte little-endian
/// prefix.
pub fn compress_prepend_size(input: &[u8]) -> Result<Vec<u8>, Lz4Error> {
    u32::try_from(input.len()).map_err(|_| Lz4Error::OutputTooLarge(input.len()))?;
    Ok(aterm_lz4::compress_prepend_size(input))
}

/// Decompress data that was produced by [`compress_prepend_size`] (or the equivalent upstream
/// `lz4_flex::compress_prepend_size`).
pub fn decompress_size_prepended(input: &[u8]) -> Result<Vec<u8>, Lz4Error> {
    if input.len() < 4 {
        return Err(Lz4Error::InputTooShort);
    }
    let original_size = u32::from_le_bytes([input[0], input[1], input[2], input[3]]) as usize;
    if original_size > MAX_DECOMPRESSED_SIZE {
        return Err(Lz4Error::OutputTooLarge(original_size));
    }
    if original_size == 0 && input.len() == 4 {
        return Ok(Vec::new());
    }
    let compressed_body_len = input.len() - 4;
    if original_size > compressed_body_len.saturating_mul(MAX_COMPRESSION_RATIO) {
        return Err(Lz4Error::CompressionBombSuspected {
            claimed_size: original_size,
            compressed_body_len,
        });
    }
    match aterm_lz4::decompress_size_prepended(input) {
        Ok(output) => {
            if output.len() != original_size {
                return Err(Lz4Error::SizeMismatch {
                    expected: original_size,
                    actual: output.len(),
                });
            }
            Ok(output)
        }
        Err(err) => Err(map_decompress_error(err)),
    }
}

const fn map_decompress_error(err: aterm_lz4::DecompressError) -> Lz4Error {
    match err {
        aterm_lz4::DecompressError::OffsetOutOfBounds => Lz4Error::InvalidOffset {
            offset: 0,
            output_pos: 0,
        },
        _ => Lz4Error::UnexpectedEof,
    }
}

#[cfg(test)]
#[path = "lz4_tests.rs"]
mod tests;
