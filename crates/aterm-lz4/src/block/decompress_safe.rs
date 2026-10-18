// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

//! The block decompression algorithm.

use crate::block::DecompressError;
use crate::block::MINMATCH;
use crate::sink::Sink;
use crate::sink::SliceSink;

use alloc::vec;
use alloc::vec::Vec;

/// Read an integer.
///
/// LZ4 lengths extend by summing bytes up to and including the first non-0xFF byte.
///
/// # Example
///
/// ```notest
///     255, 255, 255, 4, 2, 3, 4, 6, 7
/// ```
///
/// is encoded to _255 + 255 + 255 + 4 = 769_. The bytes after the first 4 is ignored, because
/// 4 is the first non-0xFF byte.
#[inline]
fn read_integer(input: &[u8], input_pos: &mut usize) -> Result<u32, DecompressError> {
    let mut n: u32 = 0;
    loop {
        // A match, not `.ok_or(..)?`: `Option::ok_or` is a std absent callee
        // the verifier cannot prove panic-free.
        let extra: u8 = match input.get(*input_pos) {
            Some(&byte) => byte,
            None => return Err(DecompressError::ExpectedAnotherByte),
        };
        // Never saturates: the `get` proved `*input_pos < input.len()`.
        *input_pos = input_pos.saturating_add(1);
        // A corrupt long `0xFF` run saturates; the caller's bounds checks
        // then reject the length.
        n = n.saturating_add(extra as u32);

        if extra != 0xFF {
            break;
        }
    }

    Ok(n)
}

/// Read a little-endian 16-bit integer from the input stream.
#[inline]
fn read_u16(input: &[u8], input_pos: &mut usize) -> Result<u16, DecompressError> {
    // A cursor into `input`: `+ 2` never saturates.
    let end = input_pos.saturating_add(2);
    // Byte `get`s, not `ok_or` + `try_into` (std absent callees).
    let lo = match input.get(*input_pos) {
        Some(&byte) => byte,
        None => return Err(DecompressError::ExpectedAnotherByte),
    };
    let hi = match input.get(input_pos.saturating_add(1)) {
        Some(&byte) => byte,
        None => return Err(DecompressError::ExpectedAnotherByte),
    };
    *input_pos = end;
    Ok(u16::from_le_bytes([lo, hi]))
}

const FIT_TOKEN_MASK_LITERAL: u8 = 0b00001111;
const FIT_TOKEN_MASK_MATCH: u8 = 0b11110000;

/// True when both token nibbles (literal length high, match length low) are below 15, so no
/// extension bytes follow.
#[inline]
fn does_token_fit(token: u8) -> bool {
    !((token & FIT_TOKEN_MASK_LITERAL) == FIT_TOKEN_MASK_LITERAL
        || (token & FIT_TOKEN_MASK_MATCH) == FIT_TOKEN_MASK_MATCH)
}

/// Decompress all bytes of `input` into `output`.
///
/// Returns the number of bytes written (decompressed) into `output`.
#[inline(always)]
// `always`: best performance in non-LTO builds.
// Skip: the fast path's 16-byte `try_into().unwrap()` read is in bounds by the
// `input_pos <= safe_input_pos` guard (len - 16 - 2), which the verifier cannot
// follow.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn decompress_internal<const USE_DICT: bool, S: Sink>(
    input: &[u8],
    output: &mut S,
    ext_dict: &[u8],
) -> Result<usize, DecompressError> {
    let mut input_pos = 0;
    let initial_output_pos = output.pos();

    let safe_input_pos = input
        .len()
        .saturating_sub(16 /* literal copy */ +  2 /* u16 match offset */);
    let mut safe_output_pos = output
        .capacity()
        .saturating_sub(16 /* literal copy */ + 18 /* match copy */);

    if USE_DICT {
        // In the dictionary case the output pointer is moved by the match length in the dictionary.
        // This may be up to 17 bytes without exiting the loop. So we need to ensure that we have
        // at least additional 17 bytes of space left in the output buffer in the fast loop.
        safe_output_pos = safe_output_pos.saturating_sub(17);
    };

    // Exhaust the decoder by reading and decompressing all blocks until the remaining buffer is
    // empty.
    loop {
        // The token: literal length in the high nibble, match length in the low.
        let token = *input
            .get(input_pos)
            .ok_or(DecompressError::ExpectedAnotherByte)?;
        input_pos += 1;

        // Fast path: the common token fits in one byte and we are far from the end.
        // `<` (not `<=`) on the output side covers a `safe_output_pos` saturated to 0.
        if does_token_fit(token) && input_pos <= safe_input_pos && output.pos() < safe_output_pos {
            let literal_length = (token >> 4) as usize;

            let input: &[u8; 16] = input[input_pos..input_pos + 16].try_into().unwrap();

            // The literal is at most 14 bytes; the guard leaves room to copy 16.
            output.extend_from_slice_wild(input, literal_length);
            input_pos += literal_length;

            // clone as we don't want to mutate
            let offset = read_u16(input, &mut literal_length.clone())? as usize;
            input_pos += 2;

            let mut match_length = MINMATCH + (token & 0xF) as usize;

            if USE_DICT && offset > output.pos() {
                let copied = copy_from_dict(output, ext_dict, offset, match_length)?;
                if copied == match_length {
                    continue;
                }
                // match crosses ext_dict and output, offset is still correct as output pos
                // increased
                match_length -= copied;
            }

            // In this branch we know that match_length is at most 18 (14 + MINMATCH).
            // But the blocks can overlap, so make sure they are at least 18 bytes apart
            // to enable an optimized copy of 18 bytes.
            let start = output.pos().saturating_sub(offset);
            if offset >= match_length {
                output.extend_from_within(start, 18, match_length);
            } else {
                output.extend_from_within_overlapping(start, match_length)
            }

            continue;
        }

        // Literal section; a nibble of 15 means extension bytes follow.
        let mut literal_length = (token >> 4) as usize;
        if literal_length != 0 {
            if literal_length == 15 {
                literal_length += read_integer(input, &mut input_pos)? as usize;
            }

            if literal_length > input.len() - input_pos {
                return Err(DecompressError::LiteralOutOfBounds);
            }
            if literal_length > output.capacity() - output.pos() {
                return Err(DecompressError::OutputTooSmall {
                    expected: output.pos() + literal_length,
                    actual: output.capacity(),
                });
            }
            output.extend_from_slice(&input[input_pos..input_pos + literal_length]);
            input_pos += literal_length;
        }

        // Input exhausted: the last sequence has no match.
        if input_pos >= input.len() {
            break;
        }

        let offset = read_u16(input, &mut input_pos)? as usize;
        // Match length: MINMATCH plus the low nibble; 19 means extension bytes follow.
        let mut match_length = MINMATCH + (token & 0xF) as usize;
        if match_length == MINMATCH + 15 {
            match_length += read_integer(input, &mut input_pos)? as usize;
        }

        if output.pos() + match_length > output.capacity() {
            return Err(DecompressError::OutputTooSmall {
                expected: output.pos() + match_length,
                actual: output.capacity(),
            });
        }
        if USE_DICT && offset > output.pos() {
            let copied = copy_from_dict(output, ext_dict, offset, match_length)?;
            if copied == match_length {
                continue;
            }
            // match crosses ext_dict and output, offset is still correct as output_len was
            // increased
            match_length -= copied;
        }
        // Copy the match from already-decompressed output.
        duplicate_slice(output, offset, match_length)?;
    }
    Ok(output.pos() - initial_output_pos)
}

#[inline]
// Skip: one residual panic-freedom row the verifier cannot transport; the
// arithmetic is guarded and the `get` fails closed.
#[cfg_attr(trust_verify, trust::skip)]
fn copy_from_dict(
    output: &mut impl Sink,
    ext_dict: &[u8],
    offset: usize,
    match_length: usize,
) -> Result<usize, DecompressError> {
    // If we're here we know offset > output.pos
    debug_assert!(offset > output.pos());
    let (back, back_overflow) = offset.overflowing_sub(output.pos());
    if back_overflow {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    let (dict_offset, did_overflow) = ext_dict.len().overflowing_sub(back);
    if did_overflow {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    // The match may cross from dict into output; `back == len - dict_offset`.
    let dict_match_length = match_length.min(back);
    // In bounds (`<= len`), but the verifier loses that relation: `get`
    // needs no proof and fails closed, as does a (impossible) wrap.
    let ext_match = match ext_dict.get(dict_offset..dict_offset.wrapping_add(dict_match_length)) {
        Some(slice) => slice,
        None => return Err(DecompressError::OffsetOutOfBounds),
    };
    output.extend_from_slice(ext_match);
    Ok(dict_match_length)
}

/// Extends output by self-referential copies
#[inline(always)] // (always) necessary otherwise compiler fails to inline it
fn duplicate_slice(
    output: &mut impl Sink,
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    // This function assumes output will fit match_length, it might panic otherwise.
    if match_length > offset {
        duplicate_overlapping_slice(output, offset, match_length)?;
    } else {
        let (start, did_overflow) = output.pos().overflowing_sub(offset);
        if did_overflow {
            return Err(DecompressError::OffsetOutOfBounds);
        }

        // `pos() + 64` cannot overflow; saturating is for the verifier.
        match match_length {
            0..=32 if output.pos().saturating_add(32) <= output.capacity() => {
                output.extend_from_within(start, 32, match_length)
            }
            33..=64 if output.pos().saturating_add(64) <= output.capacity() => {
                output.extend_from_within(start, 64, match_length)
            }
            _ => output.extend_from_within(start, match_length, match_length),
        }
    }
    Ok(())
}

/// Self-referential copy where the match overlaps the bytes it writes.
#[inline]
fn duplicate_overlapping_slice(
    sink: &mut impl Sink,
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    // This function assumes output will fit match_length, it might panic otherwise.
    let (start, did_overflow) = sink.pos().overflowing_sub(offset);
    if did_overflow {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    if offset == 1 {
        let val = sink.byte_at(start);
        sink.extend_with_fill(val, match_length);
    } else {
        sink.extend_from_within_overlapping(start, match_length);
    }
    Ok(())
}

/// Decompress all bytes of `input` into `output`.
/// `output` should be preallocated to the uncompressed size.
#[inline]
pub fn decompress_into(input: &[u8], output: &mut [u8]) -> Result<usize, DecompressError> {
    decompress_internal::<false, _>(input, &mut SliceSink::new(output, 0), b"")
}

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in
/// little endian. Can be used in conjunction with `compress_prepend_size`
#[inline]
pub fn decompress_size_prepended(input: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let (uncompressed_size, input) = super::uncompressed_size(input)?;
    decompress(input, uncompressed_size)
}

/// Decompress all bytes of `input` into a new vec.
/// The passed parameter `min_uncompressed_size` needs to be equal or larger than the uncompressed size.
///
/// # Panics
/// May panic if the parameter `min_uncompressed_size` is smaller than the
/// uncompressed data.
#[cfg_attr(trust_verify, trust::skip)]
// allocation panic (vec!)
// #[inline] so callers' Trust gates (aterm-scrollback) verify this body
// instead of assuming an absent callee.
#[inline]
pub fn decompress(input: &[u8], min_uncompressed_size: usize) -> Result<Vec<u8>, DecompressError> {
    let mut decompressed: Vec<u8> = vec![0; min_uncompressed_size];
    let decomp_len =
        decompress_internal::<false, _>(input, &mut SliceSink::new(&mut decompressed, 0), b"")?;
    decompressed.truncate(decomp_len);
    Ok(decompressed)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn corrupt_integer_run_errors_without_panic() {
        // A long `0xFF` length-extension run must decode to `Err`, not overflow or panic.
        let mut corrupt = vec![0xF0u8]; // token: literal nibble 15 ⇒ read extra integer
        corrupt.extend(std::iter::repeat_n(0xFFu8, 4096));
        let result = decompress(&corrupt, 64);
        assert!(
            result.is_err(),
            "corrupt 0xFF run must error, got Ok(len {:?})",
            result.map(|v| v.len())
        );

        // Token 0xFF keeps reading 0xFF extension bytes past the 64-byte input.
        assert!(
            decompress(&[0xFFu8; 64], 64).is_err(),
            "pure 0xFF garbage must decode to Err, not panic or wrongly Ok"
        );
        let _ = decompress(&[], 0);
    }

    #[test]
    fn offset_oob() {
        decompress(&[0x10, b'a', 2, 0], 4).unwrap_err();
        decompress(&[0x40, b'a', 1, 0], 4).unwrap_err();
    }
}
