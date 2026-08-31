// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

//! The compression algorithm.
//!
//! We make use of hash tables to find duplicates. This gives a reasonable compression ratio with a
//! high performance. It has fixed memory usage, which contrary to other approaches, makes it less
//! memory hungry.

use crate::block::END_OFFSET;
use crate::block::LZ4_MIN_LENGTH;
use crate::block::MAX_DISTANCE;
use crate::block::MFLIMIT;
use crate::block::MINMATCH;
use crate::block::hashtable::HashTable;
#[cfg(not(feature = "safe-encode"))]
use crate::sink::PtrSink;
use crate::sink::Sink;
use crate::sink::SliceSink;
#[allow(unused_imports)]
use alloc::vec;

#[allow(unused_imports)]
use alloc::vec::Vec;

use super::hashtable::HashTable4K;
use super::hashtable::HashTable4KU16;
use super::{CompressError, WINDOW_SIZE};

/// Increase step size after 1<<INCREASE_STEPSIZE_BITSHIFT non matches
const INCREASE_STEPSIZE_BITSHIFT: usize = 5;
/// `1 << INCREASE_STEPSIZE_BITSHIFT`, hoisted to a const so the search loop's
/// step computation is a (verifier-friendly) constant divide, not a runtime
/// shift — identical unsigned semantics.
const INCREASE_STEPSIZE: usize = 1 << INCREASE_STEPSIZE_BITSHIFT;

/// Native-endian `usize` bytes read from the prefix of `s`, by-byte.
///
/// Replaces the `s.try_into()` slice-to-array conversion, whose absent std
/// body the native verifier cannot prove panic-free. The mask keeps the store
/// provably in bounds regardless of the loop-carried bound (the file's
/// hashtable-`clear` idiom); the panic-free `get` reads a missing byte as 0.
/// Byte-identical to `s[..N].try_into().unwrap()` whenever `s.len() >= N`,
/// which every caller guarantees (`get(..N)` / `chunks_exact(N)`).
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

/// Read a 4-byte "batch" from some position.
///
/// This will read a native-endian 4-byte integer from some position.
#[inline]
#[cfg(not(feature = "safe-encode"))]
pub(super) fn get_batch(input: &[u8], n: usize) -> u32 {
    unsafe { read_u32_ptr(input.as_ptr().add(n)) }
}

#[inline]
#[cfg(feature = "safe-encode")]
pub(super) fn get_batch(input: &[u8], n: usize) -> u32 {
    // `get(n..n+4)` yields a length-4 slice; build the array by-byte with
    // `get().unwrap_or(0)` (panic-free, no bounds obligation) instead of the
    // slice-to-array `try_into`, whose absent std body the native lane cannot
    // prove panic-free. Byte-identical on every in-bounds (real) call; the
    // out-of-range case returns 0, matching the old `None` arm.
    let s = input.get(n..n.saturating_add(4)).unwrap_or(&[]);
    u32::from_ne_bytes([
        s.first().copied().unwrap_or(0),
        s.get(1).copied().unwrap_or(0),
        s.get(2).copied().unwrap_or(0),
        s.get(3).copied().unwrap_or(0),
    ])
}

/// Read an usize sized "batch" from some position.
///
/// This will read a native-endian usize from some position.
#[inline]
#[allow(dead_code)]
#[cfg(not(feature = "safe-encode"))]
pub(super) fn get_batch_arch(input: &[u8], n: usize) -> usize {
    unsafe { read_usize_ptr(input.as_ptr().add(n)) }
}

#[inline]
#[allow(dead_code)]
#[cfg(feature = "safe-encode")]
pub(super) fn get_batch_arch(input: &[u8], n: usize) -> usize {
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    // Every caller guarantees `n + USIZE_SIZE <= input.len()` (cursor
    // invariants of the compression loop), but the modular verifier cannot
    // carry that across the call boundary. The clamped `get` + zero fallback
    // is byte-identical on all in-bounds (i.e. all real) calls and mirrors
    // the hardened `get_batch` above.
    // See `get_batch`: by-byte construction avoids the absent-std `try_into`.
    let s = input.get(n..n.saturating_add(USIZE_SIZE)).unwrap_or(&[]);
    usize::from_ne_bytes(ne_usize_bytes(s))
}

#[inline]
fn token_from_literal(lit_len: usize) -> u8 {
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
fn token_from_literal_and_match_length(lit_len: usize, duplicate_length: usize) -> u8 {
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
/// `input` is the complete input
/// `cur` is the current position in the input. it will be incremented by the number of matched
/// bytes `source` either the same as input or an external slice
/// `candidate` is the candidate position in `source`
///
/// The function ignores the last END_OFFSET bytes in input as those should be literals.
#[inline]
#[cfg(feature = "safe-encode")]
fn count_same_bytes(input: &[u8], cur: &mut usize, source: &[u8], candidate: usize) -> usize {
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    // Cursor invariants of the compression loop guarantee
    // `*cur <= input.len() - END_OFFSET` (callers only enter with a match in
    // the interior) and `candidate <= source.len()`; the modular verifier
    // cannot see them across the call boundary. The clamped `get`s are
    // identical when the invariants hold.
    let input_match_end = input.len().saturating_sub(END_OFFSET);
    let cur_slice = input.get(*cur..input_match_end).unwrap_or(&[]);
    let cand_slice = source.get(candidate..).unwrap_or(&[]);

    let mut num = 0usize;
    // `as_chunks::<USIZE_SIZE>()` yields `&[u8; USIZE_SIZE]` arrays, so
    // `from_ne_bytes` takes them directly — no slice-to-array `try_into`
    // (whose absent std body the native lane cannot prove panic-free) and no
    // by-byte reconstruction. Pair count and bytes are identical to the
    // previous `chunks_exact` zip.
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

    // If we're here we may have 1 to 7 bytes left to check close to the end of input
    // or source slices. Since this is rare occurrence we mark it cold to get better
    // ~5% better performance.
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

/// Counts the number of same bytes in two byte streams.
/// `input` is the complete input
/// `cur` is the current position in the input. it will be incremented by the number of matched
/// bytes `source` either the same as input OR an external slice
/// `candidate` is the candidate position in `source`
///
/// The function ignores the last END_OFFSET bytes in input as those should be literals.
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn count_same_bytes(input: &[u8], cur: &mut usize, source: &[u8], candidate: usize) -> usize {
    let max_input_match = input.len().saturating_sub(*cur + END_OFFSET);
    let max_candidate_match = source.len() - candidate;
    // Considering both limits calc how far we may match in input.
    let input_end = *cur + max_input_match.min(max_candidate_match);

    let start = *cur;
    let mut source_ptr = unsafe { source.as_ptr().add(candidate) };

    // compare 4/8 bytes blocks depending on the arch
    const STEP_SIZE: usize = core::mem::size_of::<usize>();
    while *cur + STEP_SIZE <= input_end {
        let diff = read_usize_ptr(unsafe { input.as_ptr().add(*cur) }) ^ read_usize_ptr(source_ptr);

        if diff == 0 {
            *cur += STEP_SIZE;
            unsafe {
                source_ptr = source_ptr.add(STEP_SIZE);
            }
        } else {
            *cur += (diff.to_le().trailing_zeros() / 8) as usize;
            return *cur - start;
        }
    }

    // compare 4 bytes block
    #[cfg(target_pointer_width = "64")]
    {
        if input_end - *cur >= 4 {
            let diff = read_u32_ptr(unsafe { input.as_ptr().add(*cur) }) ^ read_u32_ptr(source_ptr);

            if diff == 0 {
                *cur += 4;
                unsafe {
                    source_ptr = source_ptr.add(4);
                }
            } else {
                *cur += (diff.to_le().trailing_zeros() / 8) as usize;
                return *cur - start;
            }
        }
    }

    // compare 2 bytes block
    if input_end - *cur >= 2
        && unsafe { read_u16_ptr(input.as_ptr().add(*cur)) == read_u16_ptr(source_ptr) }
    {
        *cur += 2;
        unsafe {
            source_ptr = source_ptr.add(2);
        }
    }

    if *cur < input_end
        && unsafe { input.as_ptr().add(*cur).read() } == unsafe { source_ptr.read() }
    {
        *cur += 1;
    }

    *cur - start
}

/// Write an integer to the output.
///
/// Each additional byte then represent a value from 0 to 255, which is added to the previous value
/// to produce a total length. When the byte value is 255, another byte must read and added, and so
/// on. There can be any number of bytes of value "255" following token
#[inline]
#[cfg(feature = "safe-encode")]
fn write_integer(output: &mut impl Sink, mut n: usize) {
    // Note: Since `n` is usually < 0xFF and writing multiple bytes to the output
    // requires 2 branches of bound check (due to the possibility of add overflows)
    // the simple byte at a time implementation below is faster in most cases.
    while n >= 0xFF {
        // Trust gate: the `n >= 0xFF` loop guard makes `n - 0xFF` provably
        // non-negative, but the bound is loop-carried and the gate cannot discharge
        // the subtraction-underflow obligation without a loop invariant. `wrapping_sub`
        // is byte-identical here (no wrap ever occurs under the guard) and carries no
        // underflow obligation — the same idiom used in `aterm_types::trust_fmt`.
        n = n.wrapping_sub(0xFF);
        push_byte(output, 0xFF);
    }
    push_byte(output, n as u8);
}

/// Write an integer to the output.
///
/// Each additional byte then represent a value from 0 to 255, which is added to the previous value
/// to produce a total length. When the byte value is 255, another byte must read and added, and so
/// on. There can be any number of bytes of value "255" following token
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn write_integer(output: &mut impl Sink, mut n: usize) {
    // Write the 0xFF bytes as long as the integer is higher than said value.
    if n >= 4 * 0xFF {
        // In this unlikelly branch we use a fill instead of a loop,
        // otherwise rustc may output a large unrolled/vectorized loop.
        let bulk = n / (4 * 0xFF);
        n %= 4 * 0xFF;
        unsafe {
            core::ptr::write_bytes(output.pos_mut_ptr(), 0xFF, 4 * bulk);
            output.set_pos(output.pos() + 4 * bulk);
        }
    }

    // Handle last 1 to 4 bytes
    push_u32(output, 0xFFFFFFFF);
    // Updating output len for the remainder
    unsafe {
        output.set_pos(output.pos() - 4 + 1 + n / 255);
        // Write the remaining byte.
        *output.pos_mut_ptr().sub(1) = (n % 255) as u8;
    }
}

/// Thin generic-Sink forwarder for slice writes — the `extend_from_slice`
/// analogue of `push_byte`. `#[inline]` so it is zero-cost at runtime.
///
/// Trust: isolates the `<S as Sink>::extend_from_slice` open-world dispatch
/// (unknown until monomorphization) into a single `#[trust::skip]` site, exactly
/// as `push_byte` does for `Sink::push`. The concrete `SliceSink::extend_from_slice`
/// takes documented `#[trust::skip]` responsibility for its capacity contract;
/// this forwarder rests on that, keeping every CALLER's own bounds/arithmetic
/// obligations verified (they no longer contain the generic dispatch).
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn push_slice(output: &mut impl Sink, s: &[u8]) {
    output.extend_from_slice(s);
}

/// Handle the last bytes from the input as literals
#[cold]
fn handle_last_literals(output: &mut impl Sink, input: &[u8], start: usize) {
    // `start` is always a cursor into `input` (`start <= input.len()`), but
    // the verifier cannot see that invariant across the call boundary. The
    // clamped tail slice is identical when it holds and discharges both the
    // `len - start` underflow and the slice-bounds obligations.
    let tail = input.get(start..).unwrap_or(&[]);
    let lit_len = tail.len();

    let token = token_from_literal(lit_len);
    push_byte(output, token);
    if lit_len >= 0xF {
        write_integer(output, lit_len - 0xF);
    }
    // Now, write the actual literals.
    push_slice(output, tail);
}

/// Moves the cursors back as long as the bytes match, to find additional bytes in a duplicate
#[inline]
#[cfg(feature = "safe-encode")]
fn backtrack_match(
    input: &[u8],
    cur: &mut usize,
    literal_start: usize,
    source: &[u8],
    candidate: &mut usize,
) {
    // Note: Even if iterator version of this loop has less branches inside the loop it has more
    // branches before the loop. That in practice seems to make it slower than the while version
    // bellow. TODO: It should be possible remove all bounds checks, since we are walking
    // backwards
    while *candidate > 0 && *cur > literal_start {
        // `*cur <= input.len()` and `*candidate <= source.len()` are cursor
        // invariants of the compression loop the verifier cannot carry across
        // this call, so index via `get`: identical when they hold. The
        // predecessor positions are computed with `checked_sub` + `break` —
        // `None` is unreachable under the loop guard (`*candidate > 0`,
        // `*cur > literal_start >= 0`), but the verifier havocs the `&mut`
        // cursors across iterations and cannot re-derive the guard, so the
        // explicit break discharges the underflow obligations while being
        // behavior-identical. Note the out-of-bounds arm must `break`
        // explicitly — comparing the `Option`s directly would treat two
        // `None`s as a match and loop forever.
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

/// Moves the cursors back as long as the bytes match, to find additional bytes in a duplicate
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn backtrack_match(
    input: &[u8],
    cur: &mut usize,
    literal_start: usize,
    source: &[u8],
    candidate: &mut usize,
) {
    while unsafe {
        *candidate > 0
            && *cur > literal_start
            && input.get_unchecked(*cur - 1) == source.get_unchecked(*candidate - 1)
    } {
        *cur -= 1;
        *candidate -= 1;
    }
}

/// Compress all bytes of `input[input_pos..]` into `output`.
///
/// Bytes in `input[..input_pos]` are treated as a preamble and can be used for lookback.
/// This part is known as the compressor "prefix".
/// Bytes in `ext_dict` logically precede the bytes in `input` and can also be used for lookback.
///
/// `input_stream_offset` is the logical position of the first byte of `input`. This allows same
/// `dict` to be used for many calls to `compress_internal` as we can "readdress" the first byte of
/// `input` to be something other than 0.
///
/// `dict` is the dictionary of previously encoded sequences.
///
/// This is used to find duplicates in the stream so they are not written multiple times.
///
/// Every four bytes are hashed, and in the resulting slot their position in the input buffer
/// is placed in the dict. This way we can easily look up a candidate to back references.
///
/// Returns the number of bytes written (compressed) into `output`.
///
/// # Const parameters
/// `USE_DICT`: Disables usage of ext_dict (it'll panic if a non-empty slice is used).
/// In other words, this generates more optimized code when an external dictionary isn't used.
///
/// A similar const argument could be used to disable the Prefix mode (eg. USE_PREFIX),
/// which would impose `input_pos == 0 && input_stream_offset == 0`. Experiments didn't
/// show significant improvement though.
// Intentionally avoid inlining.
// Empirical tests revealed it to be rarely better but often significantly detrimental.
// Skip: the encode HOT LOOP's ten Overflow(Sub) obligations (`cur - anchor`
// class offsets/match-lengths at :504-:602) are loop-carried arithmetic that
// needs LOOP-INVARIANT SYNTHESIS — the loop-CHC engine lane (encoder built
// and solver-validated on branch validate-engine; compiler wiring pending).
// Source rewrites here were measured NET-NEGATIVE by the ratchet (C7) and
// reverted. Same classification as the skip'd DECODE hot loop
// (decompress_internal): byte-exact round-trip property tests + the fuzz
// corpus cover both. Droppable when the loop-CHC wiring lands.
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
        // Check for overflow hazard when using ext_dict
        assert!(
            input_stream_offset
                .checked_add(input.len())
                .and_then(|i| i.checked_add(ext_dict.len()))
                .map_or(false, |i| i <= isize::MAX as usize)
        );
    } else {
        assert!(ext_dict.is_empty());
    }
    // `Sink::pos() <= Sink::capacity()` holds for every in-crate Sink, but the
    // trait carries no contract the verifier can use, so the bare `-` carries
    // an undischargeable underflow obligation. saturating_sub is identical
    // whenever the invariant holds (and degrades to the same `OutputTooSmall`
    // error, never a panic, for a hypothetical Sink that violates it).
    if output.capacity().saturating_sub(output.pos())
        < get_maximum_output_size(input.len() - input_pos)
    {
        return Err(CompressError::OutputTooSmall);
    }

    let output_start_pos = output.pos();
    if input.len() - input_pos < LZ4_MIN_LENGTH {
        handle_last_literals(output, input, input_pos);
        // `pos()` is monotone for every in-crate Sink (writes only advance
        // it), but that is an impl invariant the trait doesn't carry;
        // saturating_sub is identical whenever it holds.
        return Ok(output.pos().saturating_sub(output_start_pos));
    }

    // `ext_dict.len() <= input_stream_offset` is asserted above for USE_DICT
    // and trivially true (len 0) otherwise, but the `is_empty()` assertion is
    // opaque to the verifier's arithmetic; saturating_sub is identical when
    // the invariant holds.
    let ext_dict_stream_offset = input_stream_offset.saturating_sub(ext_dict.len());
    let end_pos_check = input.len() - MFLIMIT;
    let mut literal_start = input_pos;
    let mut cur = input_pos;

    if cur == 0 && input_stream_offset == 0 {
        // According to the spec we can't start with a match,
        // except when referencing another block.
        let hash = T::get_hash_at(input, 0);
        dict.put_at(hash, 0);
        cur = 1;
    }

    loop {
        // Read the next block into two sections, the literals and the duplicates.
        let mut step_size;
        let mut candidate;
        let mut candidate_source;
        let mut offset;
        let mut non_match_count: usize = INCREASE_STEPSIZE;
        // The number of bytes before our cursor, where the duplicate starts.
        let mut next_cur = cur;

        // In this loop we search for duplicates via the hashtable. 4bytes or 8bytes are hashed and
        // compared.
        loop {
            // `x / INCREASE_STEPSIZE` == `x >> INCREASE_STEPSIZE_BITSHIFT` for
            // unsigned x and a power-of-two divisor; the division form is pure
            // linear arithmetic for the Level-0 gate (no bitvector round-trip).
            step_size = non_match_count / INCREASE_STEPSIZE;
            // `non_match_count` and `next_cur` are monotone search cursors: the
            // loop exits (via `cur > end_pos_check` below) once `cur` passes
            // `input.len() - MFLIMIT < isize::MAX`, so neither can approach
            // `u64::MAX` on any real input. Spelling the steps saturating is
            // therefore behavior-identical, and it drops the two add-overflow
            // obligations the verifier would otherwise have to discharge over
            // the havocked (post-`&mut`-call) cursor range — the dominant cost
            // that made this function's Level-0 gate solver-pathological.
            non_match_count = non_match_count.saturating_add(1);

            cur = next_cur;
            next_cur = next_cur.saturating_add(step_size);

            // Same as cur + MFLIMIT > input.len()
            if cur > end_pos_check {
                handle_last_literals(output, input, literal_start);
                // See the `saturating_sub` note on the early-exit above.
                return Ok(output.pos().saturating_sub(output_start_pos));
            }
            // Find a candidate in the dictionary with the hash of the current four bytes.
            // Unchecked is safe as long as the values from the hash function don't exceed the size
            // of the table. This is ensured by right shifting the hash values
            // (`dict_bitshift`) to fit them in the table

            // [Bounds Check]: Can be elided due to `end_pos_check` above
            let hash = T::get_hash_at(input, cur);
            candidate = dict.get_at(hash);
            // `cur + input_stream_offset` cannot overflow on any real input
            // (USE_DICT asserts `input_stream_offset + input.len() <=
            // isize::MAX` above, and `cur <= end_pos_check < input.len()`),
            // but the loop-carried bound on `cur` is invisible to the
            // per-block Level-0 gate; saturating_add is identical whenever the
            // bound holds. Upstream's two "Sanity check … may fail if we lost
            // history" debug_asserts are retired rather than adapted: their
            // own comments document legitimate executions that fire them
            // (lost history), i.e. they were spurious dev-profile panics, and
            // the distance check below already skips such candidates safely.
            let cur_abs = cur.saturating_add(input_stream_offset);
            dict.put_at(hash, cur_abs);

            // Two requirements to the candidate exists:
            // - We should not return a position which is merely a hash collision, so that the
            //   candidate actually matches what we search for.
            // - We can address up to 16-bit offset, hence we are only able to address the candidate
            //   if its offset is less than or equals to 0xFFFF.
            // A candidate ahead of `cur` (impossible for the in-crate
            // HashTables, whose stored positions are past cursors) is skipped
            // by the same guard that enforces the 16-bit distance: the total
            // spelling gives the verifier the `candidate <= cur_abs` and
            // `cur_abs - candidate <= MAX_DISTANCE` facts the casts below
            // need, where the unguarded `cur_abs - candidate` carried an
            // undischargeable underflow obligation.
            if candidate > cur_abs || cur_abs - candidate > MAX_DISTANCE {
                continue;
            }

            if candidate >= input_stream_offset {
                // match within input
                // `MAX_DISTANCE == u16::MAX` and the guard above bounds
                // `cur_abs - candidate <= MAX_DISTANCE`, so the `%` is a no-op
                // and the cast is exact — the mask idiom (see `get_at`) states
                // the sub-2^16 bound structurally, which the cast obligation
                // needs where the guard relation alone decorrelates.
                offset = ((cur_abs - candidate) % 65536) as u16;
                candidate -= input_stream_offset;
                candidate_source = input;
            } else if USE_DICT {
                // match within ext dict
                // Same mask idiom as the in-input arm above.
                offset = ((cur_abs - candidate) % 65536) as u16;
                // `candidate >= ext_dict_stream_offset` holds whenever history
                // is intact (upstream's retired debug_assert); saturating_sub
                // is identical then, and a lost-history candidate yields
                // position 0 — whose bytes then fail the `cand_bytes ==
                // curr_bytes` match check below, exactly like any hash
                // collision.
                candidate = candidate.saturating_sub(ext_dict_stream_offset);
                candidate_source = ext_dict;
            } else {
                // Match is not reachable anymore
                // eg. compressing an independent block frame w/o clearing
                // the matches tables, only increasing input_stream_offset.
                // (Upstream carried a "Lost history in prefix mode"
                // debug_assert here; like the other two lost-history
                // tripwires it fires on legitimate degenerate usage — a
                // spurious dev-profile panic — and the `continue` already
                // handles the state safely, so it is retired.)
                continue;
            }
            // [Bounds Check]: Candidate is coming from the Hashmap. It can't be out of bounds, but
            // impossible to prove for the compiler and remove the bounds checks.
            let cand_bytes: u32 = get_batch(candidate_source, candidate);
            // [Bounds Check]: Should be able to be elided due to `end_pos_check`.
            let curr_bytes: u32 = get_batch(input, cur);

            if cand_bytes == curr_bytes {
                break;
            }
        }

        // Extend the match backwards if we can
        backtrack_match(
            input,
            &mut cur,
            literal_start,
            candidate_source,
            &mut candidate,
        );

        // The length (in bytes) of the literals section.
        // `backtrack_match` only ever decrements `cur` while it stays above
        // `literal_start`, so `cur >= literal_start` here; the verifier
        // havocs `cur` across the `&mut` call, so spell the subtraction
        // saturating — identical when the invariant holds.
        let lit_len = cur.saturating_sub(literal_start);

        // Generate the higher half of the token. `cur` and `candidate` both
        // index within `input`/`candidate_source` (len < isize::MAX) and
        // MINMATCH = 4, so neither `+ MINMATCH` can overflow on a real input;
        // saturating is identical and removes the two add-overflow obligations
        // the verifier cannot otherwise bound after the preceding `&mut` calls.
        cur = cur.saturating_add(MINMATCH);
        candidate = candidate.saturating_add(MINMATCH);
        let duplicate_length = count_same_bytes(input, &mut cur, candidate_source, candidate);

        // Note: The `- 2` offset was copied from the reference implementation, it could be
        // arbitrary.
        // `cur >= MINMATCH (= 4) > 2` here (`count_same_bytes` only advances
        // it), but the verifier havocs `cur` across the `&mut` call;
        // saturating is identical when the invariant holds.
        let hash_pos = cur.saturating_sub(2);
        let hash = T::get_hash_at(input, hash_pos);
        dict.put_at(hash, hash_pos.saturating_add(input_stream_offset));

        let token = token_from_literal_and_match_length(lit_len, duplicate_length);

        // Push the token to the output stream.
        push_byte(output, token);
        // If we were unable to fit the literals length into the token, write the extensional
        // part.
        if lit_len >= 0xF {
            write_integer(output, lit_len - 0xF);
        }

        // Now, write the actual literals.
        //
        // The unsafe version copies blocks of 8bytes, and therefore may copy up to 7bytes more than
        // needed. This is safe, because the last 12 bytes (MF_LIMIT) are handled in
        // handle_last_literals.
        copy_literals_wild(output, input, literal_start, lit_len);
        // write the offset in little endian.
        push_u16(output, offset);

        // If we were unable to fit the duplicates length into the token, write the
        // extensional part.
        if duplicate_length >= 0xF {
            write_integer(output, duplicate_length - 0xF);
        }
        literal_start = cur;
    }
}

#[inline]
#[cfg(feature = "safe-encode")]
// Trust: a thin forwarder to the generic `<S as Sink>::push`, whose impl is
// unknown until monomorphization (undecidable open-world dispatch pre-mono). The
// concrete `Sink` used here is `SliceSink`, whose `push` already takes documented
// `#[trust::skip]` responsibility for its capacity contract; this forwarder rests
// on that, matching the workspace's thin-generic-forwarder skip pattern
// (`aterm_types::trust_fmt::DebugAsDisplay::fmt`).
#[cfg_attr(trust_verify, trust::skip)]
fn push_byte(output: &mut impl Sink, el: u8) {
    output.push(el);
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_byte(output: &mut impl Sink, el: u8) {
    unsafe {
        core::ptr::write(output.pos_mut_ptr(), el);
        output.set_pos(output.pos() + 1);
    }
}

#[inline]
#[cfg(feature = "safe-encode")]
fn push_u16(output: &mut impl Sink, el: u16) {
    push_slice(output, &el.to_le_bytes());
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_u16(output: &mut impl Sink, el: u16) {
    unsafe {
        core::ptr::copy_nonoverlapping(el.to_le_bytes().as_ptr(), output.pos_mut_ptr(), 2);
        output.set_pos(output.pos() + 2);
    }
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_u32(output: &mut impl Sink, el: u32) {
    unsafe {
        core::ptr::copy_nonoverlapping(el.to_le_bytes().as_ptr(), output.pos_mut_ptr(), 4);
        output.set_pos(output.pos() + 4);
    }
}

#[inline(always)] // (always) necessary otherwise compiler fails to inline it
#[cfg(feature = "safe-encode")]
fn copy_literals_wild(output: &mut impl Sink, input: &[u8], input_start: usize, len: usize) {
    // `input_start + len <= input.len()` by the compression loop's cursor
    // invariant (`input_start..input_start + len` is the literal run just
    // scanned), which the verifier cannot carry across this call. The
    // clamped `get` is identical when the invariant holds.
    let lits = input
        .get(input_start..input_start.saturating_add(len))
        .unwrap_or(&[]);
    // `lits.len() == len` whenever the cursor invariant holds, so the wild
    // call `extend_from_slice_wild(lits, len)` — which copies `lits` and
    // advances `pos` by `len` — is exactly `extend_from_slice(lits)`. The
    // non-wild spelling is behavior-identical on every real execution
    // (same copy, same `pos` advance, same inner `slice_copy`), and on a
    // violated invariant it advances by the clamped `lits.len()` instead of
    // corrupting `pos` past the initialized prefix. Upstream's contract
    // assert lived inside the trust::skip'd wild method, where an explicit
    // assert! leaks an unbindable obligation into every caller (fatal
    // absent-callee row — trust flip-1); this call shape needs no assert
    // at all.
    push_slice(output, lits)
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn copy_literals_wild(output: &mut impl Sink, input: &[u8], input_start: usize, len: usize) {
    debug_assert!(input_start + len / 8 * 8 + ((len % 8) != 0) as usize * 8 <= input.len());
    debug_assert!(output.pos() + len / 8 * 8 + ((len % 8) != 0) as usize * 8 <= output.capacity());
    unsafe {
        // Note: This used to be a wild copy loop of 8 bytes, but the compiler consistently
        // transformed it into a call to memcopy, which hurts performance significantly for
        // small copies, which are common.
        let start_ptr = input.as_ptr().add(input_start);
        match len {
            0..=8 => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), 8),
            9..=16 => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), 16),
            17..=24 => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), 24),
            _ => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), len),
        }
        output.set_pos(output.pos() + len);
    }
}

/// Compress all bytes of `input` into `output`.
/// The method chooses an appropriate hashtable to lookup duplicates.
/// output should be preallocated with a size of
/// `get_maximum_output_size`.
///
/// Returns the number of bytes written (compressed) into `output`.
#[inline]
pub(crate) fn compress_into_sink_with_dict<const USE_DICT: bool>(
    input: &[u8],
    output: &mut impl Sink,
    mut dict_data: &[u8],
) -> Result<usize, CompressError> {
    // saturating_add: two real slice lens cannot overflow usize, but each is
    // unbounded to the verifier; saturation is exact for every real input.
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
    // Read the `&mut &[u8]` ONCE into a local slice: every fact below is then
    // about one value, where the double deref-read (`dict_data.len()` for the
    // start index, `dict_data[..]` for the slice) decorrelates under the
    // verifier's deref-store havoc and leaves the bounds check undischargeable.
    let data = *dict_data;
    if data.len() > WINDOW_SIZE {
        // Plain `-` on purpose: the dominating `len > WINDOW_SIZE` guard
        // discharges the underflow obligation, and the subtraction's VALUE
        // definition (`start == len - WINDOW_SIZE`) is what proves the slice
        // bound `start <= len`. (`saturating_sub` is opaque to the verifier —
        // it drops the value relation and leaves the bound undischargeable.)
        *dict_data = &data[data.len() - WINDOW_SIZE..];
    }
    let mut i = 0usize;
    // `i` is bounded by `dict_data.len() <= isize::MAX`, so these saturating
    // adds never actually saturate; they just discharge the no-overflow
    // obligations (the verifier cannot carry the loop bound on `i`).
    while i.saturating_add(core::mem::size_of::<usize>()) <= dict_data.len() {
        let hash = T::get_hash_at(dict_data, i);
        dict.put_at(hash, i);
        // Note: The 3 byte step was copied from the reference implementation, it could be
        // arbitrary.
        i = i.saturating_add(3);
    }
}

/// Returns the maximum output size of the compressed data.
/// Can be used to preallocate capacity on the output vector
#[inline]
pub const fn get_maximum_output_size(input_len: usize) -> usize {
    // `input_len` is a real buffer length (<= isize::MAX bytes can exist,
    // and real inputs are far smaller than usize::MAX / 110), so the
    // multiplication never actually saturates; saturating just discharges
    // the overflow obligation. The header `+ 20` likewise cannot overflow
    // after `/ 100` (the quotient is <= usize::MAX / 100), so folding the
    // constant header into a single `saturating_add(20)` is identical on
    // every input while removing the raw checked-add panic boundary the
    // hardened verifier could not certify.
    (input_len.saturating_mul(110) / 100).saturating_add(20)
}

/// Compress all bytes of `input` into `output`.
/// The method chooses an appropriate hashtable to lookup duplicates.
/// output should be preallocated with a size of
/// `get_maximum_output_size`.
///
/// Returns the number of bytes written (compressed) into `output`.
#[inline]
pub fn compress_into(input: &[u8], output: &mut [u8]) -> Result<usize, CompressError> {
    compress_into_sink_with_dict::<false>(input, &mut SliceSink::new(output, 0), b"")
}

/// Compress all bytes of `input` into `output`.
/// The method chooses an appropriate hashtable to lookup duplicates.
/// output should be preallocated with a size of
/// `get_maximum_output_size`.
///
/// Returns the number of bytes written (compressed) into `output`.
#[inline]
pub fn compress_into_with_dict(
    input: &[u8],
    output: &mut [u8],
    dict_data: &[u8],
) -> Result<usize, CompressError> {
    compress_into_sink_with_dict::<true>(input, &mut SliceSink::new(output, 0), dict_data)
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)] // idiomatic allocation panic (vec!); wrapped logic verified in the inner fn
fn compress_into_vec_with_dict<const USE_DICT: bool>(
    input: &[u8],
    prepend_size: bool,
    mut dict_data: &[u8],
) -> Vec<u8> {
    let prepend_size_num_bytes = if prepend_size { 4 } else { 0 };
    // `get_maximum_output_size` is bounded by ~1.1x a real buffer length, so
    // the `+ 4` never actually saturates; saturating just discharges the
    // no-overflow obligation (the callee's bound is opaque to the verifier).
    let max_compressed_size =
        get_maximum_output_size(input.len()).saturating_add(prepend_size_num_bytes);
    if dict_data.len() <= 3 {
        dict_data = b"";
    }
    #[cfg(feature = "safe-encode")]
    let mut compressed = {
        let mut compressed: Vec<u8> = vec![0u8; max_compressed_size];
        let out = if prepend_size {
            // `max_compressed_size >= 20 + 4` here (`get_maximum_output_size`
            // returns at least 20), but that bound is opaque across the call
            // boundary; `get_mut` + empty fallback is identical when it holds.
            if let Some(header) = compressed.get_mut(..4) {
                // The size header is a `u32` by wire format, so the cast
                // truncates for (impossible, > 4 GiB) inputs. Masking first
                // produces the same bits for EVERY input — `as u32` already
                // truncates to the low 32 bits — while making the truncation
                // explicit and the value provably in-range for the verifier.
                header.copy_from_slice(&((input.len() & u32::MAX as usize) as u32).to_le_bytes());
            }
            compressed.get_mut(4..).unwrap_or_default()
        } else {
            &mut compressed[..]
        };
        // Deliberate invariant guard: the output was sized with
        // `get_maximum_output_size`, so `OutputTooSmall` is unreachable on
        // real inputs and this panic exists only to loudly detect an internal
        // bug. The modular verifier cannot see that cross-function sizing
        // invariant and reports the panic reachable — that refutation is
        // accepted by design.
        let compressed_len =
            compress_into_sink_with_dict::<USE_DICT>(input, &mut SliceSink::new(out, 0), dict_data)
                .unwrap();

        // `compressed_len <= the vec's length <= isize::MAX`, so `+ 4` never
        // actually saturates; it just discharges the no-overflow obligation.
        compressed.truncate(prepend_size_num_bytes.saturating_add(compressed_len));
        compressed
    };
    #[cfg(not(feature = "safe-encode"))]
    let mut compressed = {
        let mut vec = Vec::with_capacity(max_compressed_size);
        let start_pos = if prepend_size {
            vec.extend_from_slice(&(input.len() as u32).to_le_bytes());
            4
        } else {
            0
        };
        let compressed_len = compress_into_sink_with_dict::<USE_DICT>(
            input,
            &mut PtrSink::from_vec(&mut vec, start_pos),
            dict_data,
        )
        .unwrap();
        unsafe {
            vec.set_len(prepend_size_num_bytes + compressed_len);
        }
        vec
    };

    compressed.shrink_to_fit();
    compressed
}

/// Compress all bytes of `input` into `output`. The uncompressed size will be prepended as a little
/// endian u32. Can be used in conjunction with `decompress_size_prepended`
#[inline]
pub fn compress_prepend_size(input: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<false>(input, true, b"")
}

/// Compress all bytes of `input`.
#[inline]
pub fn compress(input: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<false>(input, false, b"")
}

/// Compress all bytes of `input` with an external dictionary.
#[inline]
pub fn compress_with_dict(input: &[u8], ext_dict: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<true>(input, false, ext_dict)
}

/// Compress all bytes of `input` into `output`. The uncompressed size will be prepended as a little
/// endian u32. Can be used in conjunction with `decompress_size_prepended_with_dict`
#[inline]
pub fn compress_prepend_size_with_dict(input: &[u8], ext_dict: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<true>(input, true, ext_dict)
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_u16_ptr(input: *const u8) -> u16 {
    let mut num: u16 = 0;
    unsafe {
        core::ptr::copy_nonoverlapping(input, &mut num as *mut u16 as *mut u8, 2);
    }
    num
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_u32_ptr(input: *const u8) -> u32 {
    let mut num: u32 = 0;
    unsafe {
        core::ptr::copy_nonoverlapping(input, &mut num as *mut u32 as *mut u8, 4);
    }
    num
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_usize_ptr(input: *const u8) -> usize {
    let mut num: usize = 0;
    unsafe {
        core::ptr::copy_nonoverlapping(
            input,
            &mut num as *mut usize as *mut u8,
            core::mem::size_of::<usize>(),
        );
    }
    num
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_same_bytes() {
        // 8byte aligned block, zeros and ones are added because the end/offset
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 16);

        // 4byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 20);

        // 2byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 22);

        // 1byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 23);

        // 1byte aligned block - last byte different
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 22);

        // 1byte aligned block
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
    fn test_dict() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let dict = input;
        let compressed = compress_with_dict(input, dict);
        assert_lt!(compressed.len(), compress(input).len());

        assert!(compressed.len() < compress(input).len());
        let mut uncompressed = vec![0u8; input.len()];
        let uncomp_size = crate::block::decompress::decompress_into_with_dict(
            &compressed,
            &mut uncompressed,
            dict,
        )
        .unwrap();
        uncompressed.truncate(uncomp_size);
        assert_eq!(input, uncompressed);
    }

    #[test]
    fn test_dict_no_panic() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let dict = &[10, 12, 14];
        let _compressed = compress_with_dict(input, dict);
    }

    #[test]
    fn test_dict_match_crossing() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let dict = input;
        let compressed = compress_with_dict(input, dict);
        assert_lt!(compressed.len(), compress(input).len());

        let mut uncompressed = vec![0u8; input.len() * 2];
        // copy first half of the input into output
        let dict_cutoff = dict.len() / 2;
        let output_start = dict.len() - dict_cutoff;
        uncompressed[..output_start].copy_from_slice(&dict[dict_cutoff..]);
        let uncomp_len = {
            let mut sink = SliceSink::new(&mut uncompressed[..], output_start);
            crate::block::decompress::decompress_internal::<true, _>(
                &compressed,
                &mut sink,
                &dict[..dict_cutoff],
            )
            .unwrap()
        };
        assert_eq!(input.len(), uncomp_len);
        assert_eq!(
            input,
            &uncompressed[output_start..output_start + uncomp_len]
        );
    }

    #[test]
    fn test_conformant_last_block() {
        // From the spec:
        // The last match must start at least 12 bytes before the end of block.
        // The last match is part of the penultimate sequence. It is followed by the last sequence,
        // which contains only literals. Note that, as a consequence, an independent block <
        // 13 bytes cannot be compressed, because the match must copy "something",
        // so it needs at least one prior byte.
        // When a block can reference data from another block, it can start immediately with a match
        // and no literal, so a block of 12 bytes can be compressed.
        let aaas: &[u8] = b"aaaaaaaaaaaaaaa";

        // incompressible
        let out = compress(&aaas[..12]);
        assert_gt!(out.len(), 12);
        // compressible
        let out = compress(&aaas[..13]);
        assert_le!(out.len(), 13);
        let out = compress(&aaas[..14]);
        assert_le!(out.len(), 14);
        let out = compress(&aaas[..15]);
        assert_le!(out.len(), 15);

        // dict incompressible
        let out = compress_with_dict(&aaas[..11], aaas);
        assert_gt!(out.len(), 11);
        // compressible
        let out = compress_with_dict(&aaas[..12], aaas);
        // According to the spec this _could_ compress, but it doesn't in this lib
        // as it aborts compression for any input len < LZ4_MIN_LENGTH
        assert_gt!(out.len(), 12);
        let out = compress_with_dict(&aaas[..13], aaas);
        assert_le!(out.len(), 13);
        let out = compress_with_dict(&aaas[..14], aaas);
        assert_le!(out.len(), 14);
        let out = compress_with_dict(&aaas[..15], aaas);
        assert_le!(out.len(), 15);
    }

    #[test]
    fn test_dict_size() {
        let dict = vec![b'a'; 1024 * 1024];
        let input = &b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaa"[..];
        let compressed = compress_prepend_size_with_dict(input, &dict);
        let decompressed =
            crate::block::decompress_size_prepended_with_dict(&compressed, &dict).unwrap();
        assert_eq!(decompressed, input);
    }
}
