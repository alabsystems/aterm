// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

//! The compression algorithm.

use crate::block::END_OFFSET;
use crate::block::LZ4_MIN_LENGTH;
use crate::block::MAX_DISTANCE;
use crate::block::MFLIMIT;
use crate::block::MINMATCH;
use crate::block::hashtable::HashTable;
use crate::sink::Sink;
use crate::sink::SliceSink;
use alloc::vec;

use alloc::vec::Vec;

use super::hashtable::HashTable4K;
use super::hashtable::HashTable4KU16;
use super::{CompressError, WINDOW_SIZE};

/// Increase step size after 1<<INCREASE_STEPSIZE_BITSHIFT non matches
const INCREASE_STEPSIZE_BITSHIFT: usize = 5;
/// `1 << INCREASE_STEPSIZE_BITSHIFT`, so the search loop divides by a
/// constant (linear arithmetic for the verifier) instead of shifting.
const INCREASE_STEPSIZE: usize = 1 << INCREASE_STEPSIZE_BITSHIFT;

/// Native-endian `usize` bytes read from the prefix of `s`, by-byte.
#[cfg(target_pointer_width = "64")]
#[inline]
fn ne_usize_bytes(s: &[u8]) -> [u8; core::mem::size_of::<usize>()] {
    const N: usize = core::mem::size_of::<usize>();
    let mut buf = [0u8; N];
    let mut i = 0usize;
    while i < N {
        buf[i % N] = s.get(i).copied().unwrap_or(0);
        i = i.wrapping_add(1);
    }
    buf
}

#[inline]
pub(super) fn get_batch(input: &[u8], n: usize) -> u32 {
    let s = input.get(n..n.saturating_add(4)).unwrap_or(&[]);
    u32::from_ne_bytes([
        s.first().copied().unwrap_or(0),
        s.get(1).copied().unwrap_or(0),
        s.get(2).copied().unwrap_or(0),
        s.get(3).copied().unwrap_or(0),
    ])
}

#[cfg(target_pointer_width = "64")]
#[inline]
pub(super) fn get_batch_arch(input: &[u8], n: usize) -> usize {
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    let s = input.get(n..n.saturating_add(USIZE_SIZE)).unwrap_or(&[]);
    usize::from_ne_bytes(ne_usize_bytes(s))
}

#[inline]
const fn token_from_literal(lit_len: usize) -> u8 {
    if lit_len < 0xF {
        // Since we can fit the literals length into it, there is no need for saturation.
        (lit_len as u8) << 4
    } else {
        // We were unable to fit the literals into it, so we saturate to 0xF. We will later
        // write the extensional value.
        0xF0
    }
}

#[inline]
const fn token_from_literal_and_match_length(lit_len: usize, duplicate_length: usize) -> u8 {
    let mut token = if lit_len < 0xF {
        // Since we can fit the literals length into it, there is no need for saturation.
        (lit_len as u8) << 4
    } else {
        // We were unable to fit the literals into it, so we saturate to 0xF. We will later
        // write the extensional value.
        0xF0
    };

    token |= if duplicate_length < 0xF {
        // We could fit it in.
        duplicate_length as u8
    } else {
        // We were unable to fit it in, so we default to 0xF, which will later be extended.
        0xF
    };

    token
}

/// Counts the number of same bytes in two byte streams.
#[inline]
fn count_same_bytes(input: &[u8], cur: &mut usize, source: &[u8], candidate: usize) -> usize {
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    let input_match_end = input.len().saturating_sub(END_OFFSET);
    let cur_slice = input.get(*cur..input_match_end).unwrap_or(&[]);
    let cand_slice = source.get(candidate..).unwrap_or(&[]);

    let mut num = 0usize;
    let (cur_blocks, _) = cur_slice.as_chunks::<USIZE_SIZE>();
    let (cand_blocks, _) = cand_slice.as_chunks::<USIZE_SIZE>();
    for (block1, block2) in cur_blocks.iter().zip(cand_blocks) {
        let input_block = usize::from_ne_bytes(*block1);
        let match_block = usize::from_ne_bytes(*block2);

        if input_block == match_block {
            // `num` counts matched bytes inside `cur_slice`, so it is bounded
            // by a slice length and never actually saturates; saturating just
            // makes the no-overflow obligations provable.
            num = num.saturating_add(USIZE_SIZE);
        } else {
            let diff = input_block ^ match_block;
            num = num.saturating_add((diff.to_le().trailing_zeros() / 8) as usize);
            *cur = cur.saturating_add(num);
            return num;
        }
    }

    #[cold]
    fn count_same_bytes_tail(a: &[u8], b: &[u8], offset: usize) -> usize {
        a.iter()
            .zip(b)
            .skip(offset)
            .take_while(|(a, b)| a == b)
            .count()
    }
    num = num.saturating_add(count_same_bytes_tail(cur_slice, cand_slice, num));

    *cur = cur.saturating_add(num);
    num
}

/// Write an integer to the output.
#[inline]
fn write_integer(output: &mut impl Sink, mut n: usize) {
    while n >= 0xFF {
        n = n.wrapping_sub(0xFF);
        push_byte(output, 0xFF);
    }
    push_byte(output, n as u8);
}

/// `extend_from_slice` forwarder, like `push_byte` for `push`: `SliceSink`
/// requires capacity for the entire slice.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn push_slice(output: &mut impl Sink, s: &[u8]) {
    output.extend_from_slice(s);
}

/// Handle the last bytes from the input as literals
#[cold]
fn handle_last_literals(output: &mut impl Sink, input: &[u8], start: usize) {
    let tail = input.get(start..).unwrap_or(&[]);
    let lit_len = tail.len();

    let token = token_from_literal(lit_len);
    push_byte(output, token);
    if lit_len >= 0xF {
        write_integer(output, lit_len - 0xF);
    }
    push_slice(output, tail);
}

/// Moves the cursors back as long as the bytes match, to find additional bytes in a duplicate
#[inline]
fn backtrack_match(
    input: &[u8],
    cur: &mut usize,
    literal_start: usize,
    source: &[u8],
    candidate: &mut usize,
) {
    while *candidate > 0 && *cur > literal_start {
        let (cur_prev, cand_prev) = match (cur.checked_sub(1), candidate.checked_sub(1)) {
            (Some(c), Some(k)) => (c, k),
            _ => break,
        };
        match (input.get(cur_prev), source.get(cand_prev)) {
            (Some(a), Some(b)) if a == b => {
                *cur = cur_prev;
                *candidate = cand_prev;
            }
            _ => break,
        }
    }
}

/// Compress all bytes of `input[input_pos..]` into `output`.
#[cfg_attr(trust_verify, trust::skip)]
#[inline(never)]
pub(crate) fn compress_internal<T: HashTable, const USE_DICT: bool, S: Sink>(
    input: &[u8],
    input_pos: usize,
    output: &mut S,
    dict: &mut T,
    ext_dict: &[u8],
    input_stream_offset: usize,
) -> Result<usize, CompressError> {
    assert!(input_pos <= input.len());
    if USE_DICT {
        assert!(ext_dict.len() <= super::WINDOW_SIZE);
        assert!(ext_dict.len() <= input_stream_offset);
        assert!(
            input_stream_offset
                .checked_add(input.len())
                .and_then(|i| i.checked_add(ext_dict.len()))
                .map_or(false, |i| i <= isize::MAX as usize)
        );
    } else {
        assert!(ext_dict.is_empty());
    }
    if output.capacity().saturating_sub(output.pos())
        < get_maximum_output_size(input.len() - input_pos)
    {
        return Err(CompressError::OutputTooSmall);
    }

    let output_start_pos = output.pos();
    if input.len() - input_pos < LZ4_MIN_LENGTH {
        handle_last_literals(output, input, input_pos);
        return Ok(output.pos().saturating_sub(output_start_pos));
    }

    let ext_dict_stream_offset = input_stream_offset.saturating_sub(ext_dict.len());
    let end_pos_check = input.len() - MFLIMIT;
    let mut literal_start = input_pos;
    let mut cur = input_pos;

    if cur == 0 && input_stream_offset == 0 {
        let hash = T::get_hash_at(input, 0);
        dict.put_at(hash, 0);
        cur = 1;
    }

    loop {
        let mut step_size;
        let mut candidate;
        let mut candidate_source;
        let mut offset;
        let mut non_match_count: usize = INCREASE_STEPSIZE;
        let mut next_cur = cur;

        loop {
            step_size = non_match_count / INCREASE_STEPSIZE;
            non_match_count = non_match_count.saturating_add(1);

            cur = next_cur;
            next_cur = next_cur.saturating_add(step_size);

            if cur > end_pos_check {
                handle_last_literals(output, input, literal_start);
                return Ok(output.pos().saturating_sub(output_start_pos));
            }
            let hash = T::get_hash_at(input, cur);
            candidate = dict.get_at(hash);
            let cur_abs = cur.saturating_add(input_stream_offset);
            dict.put_at(hash, cur_abs);

            if candidate > cur_abs || cur_abs - candidate > MAX_DISTANCE {
                continue;
            }

            if candidate >= input_stream_offset {
                offset = ((cur_abs - candidate) % 65536) as u16;
                candidate -= input_stream_offset;
                candidate_source = input;
            } else if USE_DICT {
                offset = ((cur_abs - candidate) % 65536) as u16;
                candidate = candidate.saturating_sub(ext_dict_stream_offset);
                candidate_source = ext_dict;
            } else {
                // Match no longer reachable, e.g. tables kept across
                // independent blocks with only `input_stream_offset` growing.
                continue;
            }
            let cand_bytes: u32 = get_batch(candidate_source, candidate);
            let curr_bytes: u32 = get_batch(input, cur);

            if cand_bytes == curr_bytes {
                break;
            }
        }

        backtrack_match(
            input,
            &mut cur,
            literal_start,
            candidate_source,
            &mut candidate,
        );

        let lit_len = cur.saturating_sub(literal_start);

        cur = cur.saturating_add(MINMATCH);
        candidate = candidate.saturating_add(MINMATCH);
        let duplicate_length = count_same_bytes(input, &mut cur, candidate_source, candidate);

        let hash_pos = cur.saturating_sub(2);
        let hash = T::get_hash_at(input, hash_pos);
        dict.put_at(hash, hash_pos.saturating_add(input_stream_offset));

        let token = token_from_literal_and_match_length(lit_len, duplicate_length);

        push_byte(output, token);
        if lit_len >= 0xF {
            write_integer(output, lit_len - 0xF);
        }

        copy_literals_wild(output, input, literal_start, lit_len);
        push_u16(output, offset);

        if duplicate_length >= 0xF {
            write_integer(output, duplicate_length - 0xF);
        }
        literal_start = cur;
    }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn push_byte(output: &mut impl Sink, el: u8) {
    output.push(el);
}

#[inline]
fn push_u16(output: &mut impl Sink, el: u16) {
    push_slice(output, &el.to_le_bytes());
}

#[inline(always)]
fn copy_literals_wild(output: &mut impl Sink, input: &[u8], input_start: usize, len: usize) {
    let lits = input
        .get(input_start..input_start.saturating_add(len))
        .unwrap_or(&[]);
    push_slice(output, lits)
}

/// Compress all bytes of `input` into `output`.
#[inline]
pub(crate) fn compress_into_sink_with_dict<const USE_DICT: bool>(
    input: &[u8],
    output: &mut impl Sink,
    mut dict_data: &[u8],
) -> Result<usize, CompressError> {
    // Two real slice lengths cannot overflow; saturating is for the verifier.
    if dict_data.len().saturating_add(input.len()) < u16::MAX as usize {
        let mut dict = HashTable4KU16::new();
        init_dict(&mut dict, &mut dict_data);
        compress_internal::<_, USE_DICT, _>(input, 0, output, &mut dict, dict_data, dict_data.len())
    } else {
        let mut dict = HashTable4K::new();
        init_dict(&mut dict, &mut dict_data);
        compress_internal::<_, USE_DICT, _>(input, 0, output, &mut dict, dict_data, dict_data.len())
    }
}

#[inline]
fn init_dict<T: HashTable>(dict: &mut T, dict_data: &mut &[u8]) {
    let data = *dict_data;
    if data.len() > WINDOW_SIZE {
        // Plain `-` on purpose: the guard discharges underflow, and its value
        // proves the slice bound (`saturating_sub` would hide that relation).
        *dict_data = &data[data.len() - WINDOW_SIZE..];
    }
    let mut i = 0usize;
    while i.saturating_add(core::mem::size_of::<usize>()) <= dict_data.len() {
        let hash = T::get_hash_at(dict_data, i);
        dict.put_at(hash, i);
        i = i.saturating_add(3);
    }
}

/// Returns the maximum output size of the compressed data.
#[inline]
pub const fn get_maximum_output_size(input_len: usize) -> usize {
    // Never saturates for a real buffer length; saturating is for the verifier.
    (input_len.saturating_mul(110) / 100).saturating_add(20)
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn compress_into_vec_with_dict<const USE_DICT: bool>(
    input: &[u8],
    prepend_size: bool,
    mut dict_data: &[u8],
) -> Vec<u8> {
    let prepend_size_num_bytes = if prepend_size { 4 } else { 0 };
    let max_compressed_size =
        get_maximum_output_size(input.len()).saturating_add(prepend_size_num_bytes);
    if dict_data.len() <= 3 {
        dict_data = b"";
    }
    let mut compressed = {
        let mut compressed: Vec<u8> = vec![0u8; max_compressed_size];
        let out = if prepend_size {
            // `max_compressed_size >= 24`; `get_mut` states it for the verifier.
            if let Some(header) = compressed.get_mut(..4) {
                // The wire header is a `u32`; the mask makes the truncation
                // `as u32` already does explicit for the verifier.
                header.copy_from_slice(&((input.len() & u32::MAX as usize) as u32).to_le_bytes());
            }
            compressed.get_mut(4..).unwrap_or_default()
        } else {
            &mut compressed[..]
        };
        let compressed_len =
            compress_into_sink_with_dict::<USE_DICT>(input, &mut SliceSink::new(out, 0), dict_data)
                .unwrap();

        compressed.truncate(prepend_size_num_bytes.saturating_add(compressed_len));
        compressed
    };

    compressed.shrink_to_fit();
    compressed
}

/// Compress all bytes of `input` into `output`.
#[inline]
pub fn compress_prepend_size(input: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<false>(input, true, b"")
}

/// Compress all bytes of `input`.
#[inline]
pub fn compress(input: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<false>(input, false, b"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_same_bytes() {
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 16);

        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 20);

        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 22);

        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 23);

        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 22);

        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 9, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 21);

        for diff_idx in 8..100 {
            let first: Vec<u8> = (0u8..255).cycle().take(100 + 12).collect();
            let mut second = first.clone();
            second[diff_idx] = 255;
            for start in 0..=diff_idx {
                let same_bytes = count_same_bytes(&first, &mut start.clone(), &second, start);
                assert_eq!(same_bytes, diff_idx - start);
            }
        }
    }

    #[test]
    fn test_bug() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let _out = compress(input);
    }

    #[test]
    fn test_conformant_last_block() {
        let aaas: &[u8] = b"aaaaaaaaaaaaaaa";

        let out = compress(&aaas[..12]);
        assert!(out.len() > 12, "12 bytes: {} out", out.len());
        for n in 13..=15 {
            let out = compress(&aaas[..n]);
            assert!(out.len() <= n, "{n} bytes: {} out", out.len());
        }
    }
}
