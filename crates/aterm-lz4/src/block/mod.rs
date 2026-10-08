// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

//! LZ4 Block Format
//!
//! As defined in <https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md>.

pub(crate) mod compress;
pub(crate) mod hashtable;

// Upstream's raw-pointer `decompress.rs` is not carried; the bounds-checked
// decoder takes its name.
pub(crate) mod decompress_safe;
pub(crate) use decompress_safe as decompress;

pub use compress::*;
pub use decompress::*;

use core::{error::Error, fmt};

pub(crate) const WINDOW_SIZE: usize = 64 * 1024;

/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// The last match must start at least 12 bytes before the end of block. The last match is part of
/// the penultimate sequence. It is followed by the last sequence, which contains only literals.
///
/// Note that, as a consequence, an independent block < 13 bytes cannot be compressed, because the
/// match must copy "something", so it needs at least one prior byte.
///
/// When a block can reference data from another block, it can start immediately with a match and no
/// literal, so a block of 12 bytes can be compressed.
const MFLIMIT: usize = 12;

/// The last 5 bytes of input are always literals. Therefore, the last sequence contains at least 5
/// bytes.
const LAST_LITERALS: usize = 5;

/// The compression loop may read up to (register_size - 2) bytes past the current position, so
/// matches end 6 bytes before the end of input: 1 more than the spec requires.
const END_OFFSET: usize = LAST_LITERALS + 1;

/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// Minimum length of a block
///
/// MFLIMIT + 1 for the token.
const LZ4_MIN_LENGTH: usize = MFLIMIT + 1;

const MAXD_LOG: usize = 16;
const MAX_DISTANCE: usize = (1 << MAXD_LOG) - 1;

/// The minimum length of a duplicate
const MINMATCH: usize = 4;

/// An error representing invalid compressed data.
#[derive(Debug)]
#[non_exhaustive]
pub enum DecompressError {
    /// The provided output is too small
    OutputTooSmall {
        /// Minimum expected output size
        expected: usize,
        /// Actual size of output
        actual: usize,
    },
    /// Literal is out of bounds of the input
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
                write!(
                    f,
                    "provided output is too small for the decompressed data, actual {actual}, expected \
                     {expected}"
                )
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

/// This can be used in conjunction with `decompress_size_prepended`.
/// It will read the first 4 bytes as little-endian encoded length, and return
/// the rest of the bytes after the length encoding.
#[inline]
pub fn uncompressed_size(input: &[u8]) -> Result<(usize, &[u8]), DecompressError> {
    // Explicit `get`s, not `input[..4].try_into()`: slice→array `try_into` is
    // a std absent callee the verifier cannot prove panic-free, and indexing
    // after a `len() < 4` guard does not discharge the bounds obligation.
    let (b0, b1, b2, b3) = match (input.first(), input.get(1), input.get(2), input.get(3)) {
        (Some(&b0), Some(&b1), Some(&b2), Some(&b3)) => (b0, b1, b2, b3),
        _ => return Err(DecompressError::ExpectedAnotherByte),
    };
    let uncompressed_size = u32::from_le_bytes([b0, b1, b2, b3]) as usize;
    // Always `Some` after the four `get`s; the match keeps it total.
    let rest = match input.get(4..) {
        Some(rest) => rest,
        None => return Err(DecompressError::ExpectedAnotherByte),
    };
    Ok((uncompressed_size, rest))
}
