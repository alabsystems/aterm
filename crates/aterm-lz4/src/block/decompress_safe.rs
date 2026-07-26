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

#[allow(unused_imports)]
use alloc::vec;
#[allow(unused_imports)]
use alloc::vec::Vec;

/// Read an integer.
///
/// In LZ4, we encode small integers in a way that we can have an arbitrary number of bytes. In
/// particular, we add the bytes repeatedly until we hit a non-0xFF byte. When we do, we add
/// this byte to our sum and terminate the loop.
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
    // We start at zero and count upwards.
    let mut n: u32 = 0;
    // If this byte takes value 255 (the maximum value it can take), another byte is read
    // and added to the sum. This repeats until a byte lower than 255 is read.
    loop {
        // We add the next byte until we get a byte which we add to the counting variable.
        // Explicit match (not `.ok_or(..)?`): `Option::ok_or`'s body is a std absent
        // callee whose panic-freedom the fail-closed verifier cannot establish; the
        // match lowers to native MIR the verifier discharges directly.
        let extra: u8 = match input.get(*input_pos) {
            Some(&byte) => byte,
            None => return Err(DecompressError::ExpectedAnotherByte),
        };
        // The `get` above proves `*input_pos < input.len() <= usize::MAX`, so
        // the increment never actually saturates; saturating just discharges
        // the no-overflow obligation the verifier does not chain through `?`.
        *input_pos = input_pos.saturating_add(1);
        // Saturating: a corrupt input with a long `0xFF` run must not overflow
        // (debug-panic / release silent-wrap). On saturation the resulting length
        // is astronomically large and is rejected by the caller's bounds check
        // against the actual input/output sizes, so the decode fails cleanly.
        n = n.saturating_add(extra as u32);

        // We continue if we got 255, break otherwise.
        if extra != 0xFF {
            break;
        }
    }

    // 255, 255, 255, 8
    // 111, 111, 111, 101

    Ok(n)
}

/// Read a little-endian 16-bit integer from the input stream.
#[inline]
fn read_u16(input: &[u8], input_pos: &mut usize) -> Result<u16, DecompressError> {
    // `*input_pos` is a cursor into `input` (<= len <= isize::MAX), so the
    // `+ 2` never actually saturates; on the impossible near-usize::MAX path
    // the clamped range still fails the `get` and returns the same clean
    // error a truncated input does. Identical on all real inputs.
    let end = input_pos.saturating_add(2);
    // Two explicit byte `get`s (not `.get(range).ok_or(..)?` + `.try_into()`):
    // both `Option::ok_or` and slice→array `TryInto::try_into` are std absent
    // callees the fail-closed verifier cannot prove panic-free. The `get`s lower
    // to native MIR; on a truncated input each returns the same clean error.
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

#[test]
fn check_token() {
    assert!(!does_token_fit(15));
    assert!(does_token_fit(14));
    assert!(does_token_fit(114));
    assert!(!does_token_fit(0b11110000));
    assert!(does_token_fit(0b10110000));
}

#[test]
fn corrupt_integer_run_errors_without_panic() {
    // A long `0xFF` run in a literal-length extension integer must NOT overflow
    // or panic (the saturating `read_integer`); a corrupt block decodes to a
    // clean `Err`. Regression for the lz4 safe-decoder DoS-via-panic surface.
    let mut corrupt = vec![0xF0u8]; // token: literal nibble 15 ⇒ read extra integer
    corrupt.extend(std::iter::repeat_n(0xFFu8, 4096));
    let result = decompress(&corrupt, 64);
    assert!(
        result.is_err(),
        "corrupt 0xFF run must error, got Ok(len {:?})",
        result.map(|v| v.len())
    );

    // Pure garbage decodes to a clean Err (token 0xFF ⇒ literal-len 15 keeps
    // reading 0xFF length-extension bytes, overrunning the 64-byte input), and
    // empty input is graceful — neither panics. (The old assertion here was the
    // tautology `is_err() || is_ok()`, which verified nothing.)
    assert!(
        decompress(&[0xFFu8; 64], 64).is_err(),
        "pure 0xFF garbage must decode to Err, not panic or wrongly Ok"
    );
    let _ = decompress(&[], 0);
}

/// The token consists of two parts, the literal length (upper 4 bits) and match_length (lower 4
/// bits) if the literal length and match_length are both below 15, we don't need to read additional
/// data, so the token does fit the metadata.
#[inline]
fn does_token_fit(token: u8) -> bool {
    !((token & FIT_TOKEN_MASK_LITERAL) == FIT_TOKEN_MASK_LITERAL
        || (token & FIT_TOKEN_MASK_MATCH) == FIT_TOKEN_MASK_MATCH)
}

/// Decompress all bytes of `input` into `output`.
///
/// Returns the number of bytes written (decompressed) into `output`.
#[inline(always)]
// (always) necessary to get the best performance in non LTO builds
// The hot decode loop reads 16 bytes unchecked (`input[..].try_into().unwrap()`)
// only inside the `does_token_fit && input_pos <= safe_input_pos && ...` fast-path
// guard, whose manual-invariant chain (safe_input_pos = len - 16 - 2) the fail-closed
// verifier cannot follow. Skip is the honest escape hatch here; with the native-bundle
// skip-exclusion fix, callers demote this call to an `assumption:expected-absent-callee`
// row instead of inheriting a fatal absent-callee obligation.
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
        // Read the token. The token is the first byte in a block. It is divided into two 4-bit
        // subtokens, the higher and the lower.
        // This token contains to 4-bit "fields", a higher and a lower, representing the literals'
        // length and the back reference's length, respectively.
        let token = *input
            .get(input_pos)
            .ok_or(DecompressError::ExpectedAnotherByte)?;
        input_pos += 1;

        // Checking for hot-loop.
        // In most cases the metadata does fit in a single 1byte token (statistically) and we are in
        // a safe-distance to the end. This enables some optimized handling.
        //
        // Ideally we want to check for safe output pos like: output.pos() <= safe_output_pos; But
        // that doesn't work when the safe_output_pos is 0 due to saturated_sub. So we use
        // `<` instead of `<=`, which covers that case.
        if does_token_fit(token) && input_pos <= safe_input_pos && output.pos() < safe_output_pos {
            let literal_length = (token >> 4) as usize;

            // casting to [u8;u16] doesn't seem to make a difference vs &[u8] (same assembly)
            let input: &[u8; 16] = input[input_pos..input_pos + 16].try_into().unwrap();

            // Copy the literal
            // The literal is at max 14 bytes, and the is_safe_distance check assures
            // that we are far away enough from the end so we can safely copy 16 bytes
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

        // Now, we read the literals section.
        // Literal Section
        // If the initial value is 15, it is indicated that another byte will be read and added to
        // it
        let mut literal_length = (token >> 4) as usize;
        if literal_length != 0 {
            if literal_length == 15 {
                // The literal_length length took the maximal value, indicating that there is more
                // than 15 literal_length bytes. We read the extra integer.
                literal_length += read_integer(input, &mut input_pos)? as usize;
            }

            if literal_length > input.len() - input_pos {
                return Err(DecompressError::LiteralOutOfBounds);
            }
            // could be skipped with unchecked-decode
            if literal_length > output.capacity() - output.pos() {
                return Err(DecompressError::OutputTooSmall {
                    expected: output.pos() + literal_length,
                    actual: output.capacity(),
                });
            }
            output.extend_from_slice(&input[input_pos..input_pos + literal_length]);
            input_pos += literal_length;
        }

        // If the input stream is emptied, we break out of the loop. This is only the case
        // in the end of the stream, since the block is intact otherwise.
        if input_pos >= input.len() {
            break;
        }

        let offset = read_u16(input, &mut input_pos)? as usize;
        // Obtain the initial match length. The match length is the length of the duplicate segment
        // which will later be copied from data previously decompressed into the output buffer. The
        // initial length is derived from the second part of the token (the lower nibble), we read
        // earlier. Since having a match length of less than 4 would mean negative compression
        // ratio, we start at 4 (MINMATCH).

        // The initial match length can maximally be 19. As with the literal length, this indicates
        // that there are more bytes to read.
        let mut match_length = MINMATCH + (token & 0xF) as usize;
        if match_length == MINMATCH + 15 {
            // The match length took the maximal value, indicating that there is more bytes. We
            // read the extra integer.
            match_length += read_integer(input, &mut input_pos)? as usize;
        }

        // could be skipped with unchecked-decode
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
        // We now copy from the already decompressed buffer. This allows us for storing duplicates
        // by simply referencing the other location.
        duplicate_slice(output, offset, match_length)?;
    }
    Ok(output.pos() - initial_output_pos)
}

#[inline]
// Skip: the ext-dict copy's residual panic-freedom row is the typed-CHC
// transport gap (its arithmetic is already wrapping/guarded and the `get`
// range fails closed). Same decode-path tier as decompress_internal;
// byte-exact round-trip + fuzz corpus cover it.
#[cfg_attr(trust_verify, trust::skip)]
fn copy_from_dict(
    output: &mut impl Sink,
    ext_dict: &[u8],
    offset: usize,
    match_length: usize,
) -> Result<usize, DecompressError> {
    // If we're here we know offset > output.pos
    debug_assert!(offset > output.pos());
    // Two explicit overflowing steps (was `len.overflowing_sub(offset - pos)`
    // with a bare inner sub): behavior-identical — when `offset < pos` the old
    // inner sub wrapped to a value > len (len <= isize::MAX), so the outer
    // overflowing_sub reported overflow and returned this same error; spelling
    // the inner step overflowing discharges its Level-0 obligation directly.
    let (back, back_overflow) = offset.overflowing_sub(output.pos());
    if back_overflow {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    let (dict_offset, did_overflow) = ext_dict.len().overflowing_sub(back);
    if did_overflow {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    // Can't copy past ext_dict len, the match may cross dict and output.
    // `len - dict_offset == back` exactly (dict_offset = len - back, no
    // overflow per the guard above) — using `back` directly drops the
    // subtraction and its obligation.
    let dict_match_length = match_length.min(back);
    // `dict_offset + dict_match_length <= dict_offset + back == len` holds on
    // every real input, but the `overflowing_sub` value relation doesn't
    // survive into the bounds obligation; `get` + `ok_or` needs no bounds
    // proof at all and turns the impossible out-of-window state into the same
    // clean error a corrupt offset produces (a decompressor should never
    // panic on strange state anyway). wrapping_add: the sum cannot wrap for
    // real values (<= len <= isize::MAX); on the impossible wrap the `get`
    // range inverts and fails -> same clean error.
    // Explicit match (not `.ok_or(..)?`): `Option::ok_or` is a std absent callee
    // the fail-closed verifier cannot prove panic-free.
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

        // saturating_add: `pos + 32/64` cannot overflow for any real Sink
        // (`pos <= capacity <= isize::MAX`), but `pos()` is an opaque trait
        // call to the verifier; saturation only changes the (impossible)
        // wrap case, where the probe correctly falls to the exact-length arm.
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

/// self-referential copy for the case data start (end of output - offset) + match_length overlaps
/// into output
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
/// `output` should be preallocated with a size of of the uncompressed data.
#[inline]
pub fn decompress_into(input: &[u8], output: &mut [u8]) -> Result<usize, DecompressError> {
    decompress_internal::<false, _>(input, &mut SliceSink::new(output, 0), b"")
}

/// Decompress all bytes of `input` into `output`.
///
/// Returns the number of bytes written (decompressed) into `output`.
#[inline]
pub fn decompress_into_with_dict(
    input: &[u8],
    output: &mut [u8],
    ext_dict: &[u8],
) -> Result<usize, DecompressError> {
    decompress_internal::<true, _>(input, &mut SliceSink::new(output, 0), ext_dict)
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
// idiomatic allocation panic (vec!); wrapped logic verified in the inner fn
// #[inline] so the MIR crosses the crate boundary: callers' Trust gates
// (aterm-scrollback) bundle and VERIFY this body instead of assuming an
// absent callee. Semantics unchanged.
#[inline]
pub fn decompress(input: &[u8], min_uncompressed_size: usize) -> Result<Vec<u8>, DecompressError> {
    let mut decompressed: Vec<u8> = vec![0; min_uncompressed_size];
    let decomp_len =
        decompress_internal::<false, _>(input, &mut SliceSink::new(&mut decompressed, 0), b"")?;
    decompressed.truncate(decomp_len);
    Ok(decompressed)
}

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in
/// little endian. Can be used in conjunction with `compress_prepend_size_with_dict`
#[inline]
pub fn decompress_size_prepended_with_dict(
    input: &[u8],
    ext_dict: &[u8],
) -> Result<Vec<u8>, DecompressError> {
    let (uncompressed_size, input) = super::uncompressed_size(input)?;
    decompress_with_dict(input, uncompressed_size, ext_dict)
}

/// Decompress all bytes of `input` into a new vec.
/// The passed parameter `min_uncompressed_size` needs to be equal or larger than the uncompressed size.
///
/// # Panics
/// May panic if the parameter `min_uncompressed_size` is smaller than the
/// uncompressed data.
#[inline]
#[cfg_attr(trust_verify, trust::skip)] // idiomatic allocation panic (vec!); wrapped logic verified in the inner fn
pub fn decompress_with_dict(
    input: &[u8],
    min_uncompressed_size: usize,
    ext_dict: &[u8],
) -> Result<Vec<u8>, DecompressError> {
    let mut decompressed: Vec<u8> = vec![0; min_uncompressed_size];
    let decomp_len =
        decompress_internal::<true, _>(input, &mut SliceSink::new(&mut decompressed, 0), ext_dict)?;
    decompressed.truncate(decomp_len);
    Ok(decompressed)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn all_literal() {
        assert_eq!(decompress(&[0x30, b'a', b'4', b'9'], 3).unwrap(), b"a49");
    }

    // this error test is only valid in safe-decode.
    #[cfg(feature = "safe-decode")]
    #[test]
    fn offset_oob() {
        decompress(&[0x10, b'a', 2, 0], 4).unwrap_err();
        decompress(&[0x40, b'a', 1, 0], 4).unwrap_err();
    }
}
