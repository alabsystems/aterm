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
#[inline]
fn read_integer(input: &[u8], input_pos: &mut usize) -> Result<u32, DecompressError> {
    let mut n: u32 = 0;
    loop {
        let extra: u8 = match input.get(*input_pos) {
            Some(&byte) => byte,
            None => return Err(DecompressError::ExpectedAnotherByte),
        };
        *input_pos = input_pos.saturating_add(1);
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
    let end = input_pos.saturating_add(2);
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

/// True when both token nibbles (literal length high, match length low) are below 15.
#[inline]
const fn does_token_fit(token: u8) -> bool {
    !((token & FIT_TOKEN_MASK_LITERAL) == FIT_TOKEN_MASK_LITERAL
        || (token & FIT_TOKEN_MASK_MATCH) == FIT_TOKEN_MASK_MATCH)
}

/// Decompress all bytes of `input` into `output`.
#[inline(always)]
// Skip: fast-path 16-byte read is guarded by input_pos <= safe_input_pos.
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
        safe_output_pos = safe_output_pos.saturating_sub(17);
    };

    loop {
        let token = *input
            .get(input_pos)
            .ok_or(DecompressError::ExpectedAnotherByte)?;
        input_pos += 1;

        if does_token_fit(token) && input_pos <= safe_input_pos && output.pos() < safe_output_pos {
            let literal_length = (token >> 4) as usize;

            let input: &[u8; 16] = input[input_pos..input_pos + 16].try_into().unwrap();

            output.extend_from_slice_wild(input, literal_length);
            input_pos += literal_length;

            let offset = read_u16(input, &mut literal_length.clone())? as usize;
            input_pos += 2;

            let mut match_length = MINMATCH + (token & 0xF) as usize;

            if USE_DICT && offset > output.pos() {
                let copied = copy_from_dict(output, ext_dict, offset, match_length)?;
                if copied == match_length {
                    continue;
                }
                match_length -= copied;
            }

            let start = output.pos().saturating_sub(offset);
            if offset >= match_length {
                output.extend_from_within(start, 18, match_length);
            } else {
                output.extend_from_within_overlapping(start, match_length)
            }

            continue;
        }

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

        if input_pos >= input.len() {
            break;
        }

        let offset = read_u16(input, &mut input_pos)? as usize;
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
            match_length -= copied;
        }
        duplicate_slice(output, offset, match_length)?;
    }
    Ok(output.pos() - initial_output_pos)
}

#[inline]
// Skip: arithmetic is guarded and slice indexing fails closed.
#[cfg_attr(trust_verify, trust::skip)]
fn copy_from_dict(
    output: &mut impl Sink,
    ext_dict: &[u8],
    offset: usize,
    match_length: usize,
) -> Result<usize, DecompressError> {
    debug_assert!(offset > output.pos());
    let (back, back_overflow) = offset.overflowing_sub(output.pos());
    if back_overflow {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    let (dict_offset, did_overflow) = ext_dict.len().overflowing_sub(back);
    if did_overflow {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    let dict_match_length = match_length.min(back);
    let ext_match = match ext_dict.get(dict_offset..dict_offset.wrapping_add(dict_match_length)) {
        Some(slice) => slice,
        None => return Err(DecompressError::OffsetOutOfBounds),
    };
    output.extend_from_slice(ext_match);
    Ok(dict_match_length)
}

/// Extends output by self-referential copies.
#[inline(always)] // (always) necessary otherwise compiler fails to inline it
fn duplicate_slice(
    output: &mut impl Sink,
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    if match_length > offset {
        duplicate_overlapping_slice(output, offset, match_length)?;
    } else {
        let (start, did_overflow) = output.pos().overflowing_sub(offset);
        if did_overflow {
            return Err(DecompressError::OffsetOutOfBounds);
        }

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
#[inline]
pub fn decompress_into(input: &[u8], output: &mut [u8]) -> Result<usize, DecompressError> {
    decompress_internal::<false, _>(input, &mut SliceSink::new(output, 0), b"")
}

/// Decompress all bytes of `input` into a new vec.
#[inline]
pub fn decompress_size_prepended(input: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let (uncompressed_size, input) = super::uncompressed_size(input)?;
    decompress(input, uncompressed_size)
}

/// Decompress all bytes of `input` into a new vec.
///
/// # Panics
/// May panic if the parameter `min_uncompressed_size` is smaller than the uncompressed data.
#[cfg_attr(trust_verify, trust::skip)]
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
        let _ = decompress(&[], 0);
    }

    #[test]
    fn offset_oob() {
        decompress(&[0x10, b'a', 2, 0], 4).unwrap_err();
        decompress(&[0x40, b'a', 1, 0], 4).unwrap_err();
    }
}
