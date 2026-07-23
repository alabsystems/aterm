// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! RFC 1951 (DEFLATE) + RFC 1950 (zlib) decompression — zero dependencies.
//!
//! A from-scratch, panic-free `inflate` for decompressing attacker-supplied
//! streams (the Kitty graphics `o=z` transport). Every decode is bounded by a
//! caller-supplied `max_output` ceiling, so a decompression bomb fails with
//! [`InflateError::OutputTooLarge`] instead of exhausting memory. Huffman tables
//! use the canonical count/symbol construction (Mark Adler's `puff.c`), kept
//! deliberately simple and TOTAL — every input either decodes or returns an
//! error; none panic, none allocate without bound.

/// Why a DEFLATE/zlib stream could not be decompressed. Never a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateError {
    /// The bit stream ended before the data did.
    Truncated,
    /// The 2-byte zlib header is not `CM=8`/`FCHECK`-valid, or asks for a preset
    /// dictionary (unsupported).
    BadZlibHeader,
    /// A DEFLATE block declared the reserved block type `11`.
    BadBlockType,
    /// A stored block's `LEN`/`NLEN` ones-complement check failed.
    BadStoredLength,
    /// A Huffman code did not resolve to any symbol (corrupt table or stream).
    BadSymbol,
    /// A back-reference distance is zero or points before the output start.
    BadDistance,
    /// Decompression would exceed the caller's `max_output` ceiling (anti-bomb).
    OutputTooLarge,
    /// The zlib trailer's Adler-32 did not match the decompressed data.
    BadChecksum,
}

const MAXBITS: usize = 15;

// RFC 1951 §3.2.5 — length codes 257..285.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
// RFC 1951 §3.2.5 — distance codes 0..29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
// RFC 1951 §3.2.7 — the order code-length code lengths are transmitted in.
const CLCL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// LSB-first bit reader over a byte slice (DEFLATE bit order).
///
/// Bits are served from a `u64` accumulator (`bit_buffer`) refilled a byte at a
/// time from `data`, so the slice bounds check is paid once per ~8 bytes instead
/// of once per bit. `bit_buffer` holds `bits_in_buffer` valid bits, LSB-first
/// (the next bit to consume is bit 0); `byte_pos` is the index of the next byte
/// not yet pulled into the accumulator.
///
/// The accumulator only ever holds bytes that exist in `data` (refill checks
/// `byte_pos < data.len()`), so the total bit budget is exactly `data.len() * 8`
/// — identical to a per-bit reader. A request that runs past true end-of-input
/// therefore returns [`InflateError::Truncated`] at the exact same point; there
/// is no zero-padding past the end.
struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_buffer: u64,
    bits_in_buffer: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_buffer: 0,
            bits_in_buffer: 0,
        }
    }

    /// Pull as many whole bytes as fit into the accumulator (LSB-first, DEFLATE
    /// order). Only bytes that exist in `data` are pulled, so the accumulator
    /// never contains phantom bits past end-of-input. The `<= 56` guard keeps the
    /// `<< bits_in_buffer` shift below 64 and never overflows the `u64`.
    fn refill(&mut self) {
        // Work on locals and store back once: the verifier is a modular
        // open-model checker and cannot carry `self.*` facts between separate
        // field reads, so the loop invariants (`bits <= 56`, `pos < len`) are
        // only provable on locals. Identical iteration order and results.
        let mut bits = self.bits_in_buffer;
        let mut buf = self.bit_buffer;
        let mut pos = self.byte_pos;
        while bits <= 56 {
            let Some(&byte) = self.data.get(pos) else {
                break;
            };
            // wrapping_shl masks the amount to 0..64, which is a no-op under the
            // loop guard (`bits <= 56`) — identical result, but structurally no
            // shift-overflow panic path for the verifier to refute.
            buf |= u64::from(byte).wrapping_shl(bits);
            bits += 8;
            // `pos < data.len()`, so this never actually saturates.
            pos = pos.saturating_add(1);
        }
        self.bits_in_buffer = bits;
        self.bit_buffer = buf;
        self.byte_pos = pos;
    }

    fn read_bit(&mut self) -> Result<u32, InflateError> {
        if self.bits_in_buffer == 0 {
            self.refill();
            if self.bits_in_buffer == 0 {
                return Err(InflateError::Truncated);
            }
        }
        let bit = (self.bit_buffer & 1) as u32;
        self.bit_buffer >>= 1;
        // `bits_in_buffer >= 1` here (guarded above), so this never actually
        // saturates; `saturating_sub` just makes that locally visible.
        self.bits_in_buffer = self.bits_in_buffer.saturating_sub(1);
        Ok(bit)
    }

    fn read_bits(&mut self, n: u32) -> Result<u32, InflateError> {
        // Every caller requests at most 13 bits (the largest distance-extra
        // count); the clamp replaces the former `debug_assert!(n <= 32)` with a
        // bound the verifier can use for the mask shift below. Identical for
        // every real call.
        let n = n.min(32);
        // Truncated iff fewer than `n` bits remain in the whole stream — the same
        // boundary a per-bit reader hits, because refill only pulls real bytes.
        while self.bits_in_buffer < n {
            if self.byte_pos >= self.data.len() {
                return Err(InflateError::Truncated);
            }
            self.refill();
        }
        // `n <= 32`, so `1u64 << n` is in range; for `n == 0` the mask is 0 and
        // nothing is consumed (matching the original empty-loop behaviour).
        let mask = (1u64 << n) - 1;
        let v = (self.bit_buffer & mask) as u32;
        self.bit_buffer >>= n;
        // The `while` above guarantees `bits_in_buffer >= n` on exit, so this
        // never actually saturates.
        self.bits_in_buffer = self.bits_in_buffer.saturating_sub(n);
        Ok(v)
    }

    /// Discard bits up to the next byte boundary (for stored blocks). The low
    /// `bits_in_buffer % 8` bits are the remainder of the current partial byte;
    /// dropping them realigns the accumulator without touching `data`.
    fn align_to_byte(&mut self) {
        // Single field read into a local so `drop <= bits` is locally provable
        // (the verifier cannot relate two separate reads of the same field).
        let bits = self.bits_in_buffer;
        let drop = bits % 8;
        // wrapping_shr masks the amount to 0..64 — a no-op for `drop < 8` — and
        // wrapping_sub cannot underflow since `drop = bits % 8 <= bits`; both
        // are behavior-identical and remove the shift/underflow panic paths.
        self.bit_buffer = self.bit_buffer.wrapping_shr(drop);
        self.bits_in_buffer = bits.wrapping_sub(drop);
    }

    /// Read a byte-aligned `u8` (caller must be byte-aligned, i.e. after
    /// `align_to_byte`, so `bits_in_buffer` is a multiple of 8).
    fn read_aligned_byte(&mut self) -> Result<u8, InflateError> {
        if self.bits_in_buffer >= 8 {
            let b = (self.bit_buffer & 0xff) as u8;
            self.bit_buffer >>= 8;
            self.bits_in_buffer -= 8;
            Ok(b)
        } else {
            // Aligned and the accumulator is empty: take the next byte from `data`.
            let pos = self.byte_pos;
            let b = *self.data.get(pos).ok_or(InflateError::Truncated)?;
            // `pos < data.len()` (the `get` above succeeded), so this never
            // actually saturates.
            self.byte_pos = pos.saturating_add(1);
            Ok(b)
        }
    }

    /// Read a byte-aligned little-endian `u16`.
    fn read_aligned_u16(&mut self) -> Result<u16, InflateError> {
        let lo = self.read_aligned_byte()?;
        let hi = self.read_aligned_byte()?;
        Ok(u16::from_le_bytes([lo, hi]))
    }
}

/// Canonical Huffman decode table (count/symbol form, RFC 1951 / `puff.c`).
struct Huffman {
    count: [u16; MAXBITS + 1],
    symbol: Vec<u16>,
}

impl Huffman {
    /// Build from per-symbol code lengths (0 = symbol unused).
    ///
    /// Returns the table together with the `puff.c` Kraft *residual* `left`:
    /// `left == 0` is a complete code, `left > 0` is incomplete (some prefixes
    /// are undefined), and an over-subscribed set (where `left` would go
    /// negative) is rejected outright with [`InflateError::BadSymbol`]. Callers
    /// inspect the residual to enforce RFC 1951's completeness rules
    /// (`read_dynamic_tables`); the fixed/meta tables simply discard it.
    fn new(lengths: &[u16]) -> Result<(Self, i32), InflateError> {
        // DEFLATE alphabets never exceed 288 symbols (the fixed-literal table is
        // the largest). Bounding the input length here makes every per-length
        // count fit in `u16` and every symbol index fit in `u16`, so the
        // canonical construction below is provably free of overflow and
        // out-of-bounds access. All real callers pass <= 288 lengths.
        const MAX_SYMBOLS: usize = 288;
        if lengths.len() > MAX_SYMBOLS {
            return Err(InflateError::BadSymbol);
        }
        let mut count = [0u16; MAXBITS + 1];
        for &len in lengths {
            let l = len as usize;
            if l > MAXBITS {
                return Err(InflateError::BadSymbol);
            }
            // `count[l]` counts at most `lengths.len() <= MAX_SYMBOLS` entries,
            // so this never overflows `u16`; `saturating_add` makes that visible
            // to the verifier without altering the (unreachable) overflow path.
            count[l] = count[l].saturating_add(1);
        }
        // `puff.c` Kraft check (construct()): `left` is the number of codes of
        // the current length still available, starting from one code of length
        // 0 and doubling per bit; each used length consumes `count[len]` of
        // them. A negative residual means the code is over-subscribed (more
        // codes claimed than the prefix space allows) — reject it the way the
        // reference does. A positive residual after the loop means the code is
        // incomplete; that signal is returned for the caller to police.
        let mut left: i32 = 1;
        // Iterate the array directly (`skip(1)` == elements 1..=MAXBITS of the
        // 16-element array): a `RangeInclusive` subslice loses the static
        // length under verification and refutes the slice construction.
        for &c in count.iter().skip(1) {
            // On every real path `left <= 2^15` entering the shift (it starts
            // at 1, at most doubles per iteration, and there are 15
            // iterations) and `c <= 288`, so the subtraction never actually
            // overflows; `saturating_sub` keeps that argument local (the
            // verifier drops the bound across iterations). A (unreachable)
            // saturated result is negative and rejected below, same as the
            // over-subscribed case.
            left = (left << 1).saturating_sub(i32::from(c));
            if left < 0 {
                return Err(InflateError::BadSymbol); // over-subscribed code
            }
        }
        // `offs[len + 1] = offs[len] + count[len]` for `len` in 1..=MAXBITS,
        // i.e. a running prefix sum written to `offs[2..]` (offs[0] = offs[1] =
        // 0), expressed over iterators so no index obligation remains. The
        // running sum totals at most `lengths.len() <= MAX_SYMBOLS`, so it
        // never overflows `u16` (the `saturating_add` makes that visible).
        let mut offs = [0u16; MAXBITS + 2];
        let mut acc = 0u16;
        for (dst, &c) in offs.iter_mut().skip(2).zip(count.iter().skip(1)) {
            acc = acc.saturating_add(c);
            *dst = acc;
        }
        // `total <= lengths.len() <= MAX_SYMBOLS`; the clamp is a no-op that
        // bounds the allocation for the verifier.
        let total = lengths.iter().filter(|&&l| l != 0).count().min(MAX_SYMBOLS);
        let mut symbol = vec![0u16; total];
        let mut next = offs;
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                let l = len as usize;
                // Every length was already validated `<= MAXBITS` by the
                // counting loop above; the verifier cannot carry that fact
                // between loops, so re-establish it here (never taken).
                if l > MAXBITS {
                    continue;
                }
                // `sym < lengths.len() <= MAX_SYMBOLS <= u16::MAX`, so the cast
                // is lossless. Guard the symbol-table write so an out-of-range
                // `next[l]` (only possible for an over-subscribed code length
                // set) cannot index out of bounds.
                let slot = next[l] as usize;
                if slot < symbol.len() {
                    symbol[slot] = sym as u16;
                    next[l] = next[l].saturating_add(1);
                }
            }
        }
        Ok((Self { count, symbol }, left))
    }

    /// Decode one symbol, reading one bit at a time (MSB-accumulated code).
    fn decode(&self, br: &mut BitReader) -> Result<u16, InflateError> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        // Iterate `count[1..=MAXBITS]` by value (`skip(1)` over the 16-element
        // array) instead of indexing with the range variable, which the
        // verifier cannot bound. Identical iteration order and results.
        for &c in self.count.iter().skip(1) {
            // `read_bit` yields 0 or 1; the `& 1` mask makes the `as i32`
            // cast provably lossless without changing any value.
            code |= (br.read_bit()? & 1) as i32;
            let cnt = i32::from(c);
            // Canonical-code invariant: `first <= code` at this check (both
            // start at 0, and every non-returning iteration re-establishes
            // `code >= first + cnt` before both are shifted left once), so
            // the subtraction never underflows and `index + diff` stays small
            // (`index` sums at most 15 u16 counts). The saturating forms keep
            // that argument local; a (unreachable) saturated/negative index
            // lands in `symbol.get`'s `None` arm exactly like the wrapped
            // original cast did.
            let diff = code.saturating_sub(first);
            if diff < cnt {
                let idx = usize::try_from(index.saturating_add(diff)).unwrap_or(usize::MAX);
                return self.symbol.get(idx).copied().ok_or(InflateError::BadSymbol);
            }
            index = index.saturating_add(cnt);
            first = first.saturating_add(cnt);
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::BadSymbol)
    }
}

fn fixed_lit() -> Huffman {
    // RFC 1951 §3.2.6 fixed-Huffman literal/length code lengths. Written as an
    // index `match` over `iter_mut()` rather than `lengths[a..=b].fill(n)` so the
    // Trust verifier discharges it with ZERO slice-bounds obligations — the slice
    // coercion in `Index<RangeInclusive>` otherwise loses the static length 288 and
    // the verifier cannot prove the end bounds. Byte-identical output for all 288.
    let mut lengths = [0u16; 288];
    for (sym, len) in lengths.iter_mut().enumerate() {
        *len = match sym {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8, // 280..=287
        };
    }
    // Lengths are all within range, so construction cannot fail. The fixed
    // literal/length code is complete, so its residual is discarded.
    Huffman::new(&lengths)
        .map(|(h, _left)| h)
        .unwrap_or(Huffman {
            count: [0; MAXBITS + 1],
            symbol: Vec::new(),
        })
}

fn fixed_dist() -> Huffman {
    let lengths = [5u16; 30];
    // The fixed distance code is genuinely incomplete (30 of 32 length-5 codes
    // are used; codes 30/31 are reserved), which is exactly what RFC 1951
    // §3.2.6 specifies — so its non-zero residual is expected and discarded.
    Huffman::new(&lengths)
        .map(|(h, _left)| h)
        .unwrap_or(Huffman {
            count: [0; MAXBITS + 1],
            symbol: Vec::new(),
        })
}

fn read_dynamic_tables(br: &mut BitReader) -> Result<(Huffman, Huffman), InflateError> {
    let hlit = br.read_bits(5)? as usize + 257;
    let hdist = br.read_bits(5)? as usize + 1;
    let hclen = br.read_bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 || hclen > 19 {
        return Err(InflateError::BadSymbol);
    }
    let mut cl_lengths = [0u16; 19];
    for &slot in CLCL_ORDER.iter().take(hclen) {
        // Read first (side effect must happen unconditionally, as before),
        // then store. `CLCL_ORDER` only holds indices 0..=18, so `get_mut` is
        // always `Some`; it just makes the bound locally visible. The `& 0x7`
        // mask is a no-op on a 3-bit read; it makes the narrowing `as u16`
        // cast provably lossless.
        let v = (br.read_bits(3)? & 0x7) as u16;
        if let Some(e) = cl_lengths.get_mut(slot) {
            *e = v;
        }
    }
    let (cl_huff, cl_left) = Huffman::new(&cl_lengths)?;
    // The code-length meta-code must be a complete prefix code: every bit
    // pattern the length stream can produce has to resolve to a symbol.
    if cl_left != 0 {
        return Err(InflateError::BadSymbol);
    }

    // `hlit <= 286` and `hdist <= 30` (checked above), so the length never
    // exceeds 316; the clamp is a no-op that bounds the allocation locally.
    let mut lengths = vec![0u16; (hlit + hdist).min(316)];
    let mut i = 0;
    // All `i` increments below use `saturating_add`: `i < lengths.len()` is
    // re-checked before every write, so the addition never actually
    // saturates — it only removes the (unreachable) overflow path.
    while i < lengths.len() {
        let sym = cl_huff.decode(br)?;
        match sym {
            0..=15 => {
                // `i < lengths.len()` from the `while` head; `get_mut` keeps
                // the bound local across the opaque `decode` call above (the
                // `None` arm is unreachable).
                if let Some(slot) = lengths.get_mut(i) {
                    *slot = sym;
                }
                i = i.saturating_add(1);
            }
            16 => {
                // Repeat the previous length 3..6 times.
                if i == 0 {
                    return Err(InflateError::BadSymbol);
                }
                // `1 <= i < lengths.len()` here, so the element exists; the
                // (unreachable) `None` arm fails like any other bad symbol.
                let Some(&prev) = lengths.get(i.saturating_sub(1)) else {
                    return Err(InflateError::BadSymbol);
                };
                let repeat = 3 + br.read_bits(2)? as usize;
                for _ in 0..repeat {
                    if i >= lengths.len() {
                        return Err(InflateError::BadSymbol);
                    }
                    // Guarded `< lengths.len()` directly above, so `get_mut`
                    // is always `Some`; it keeps the bound local.
                    if let Some(slot) = lengths.get_mut(i) {
                        *slot = prev;
                    }
                    i = i.saturating_add(1);
                }
            }
            17 => {
                // Repeat zero 3..10 times.
                let repeat = 3 + br.read_bits(3)? as usize;
                for _ in 0..repeat {
                    if i >= lengths.len() {
                        return Err(InflateError::BadSymbol);
                    }
                    if let Some(slot) = lengths.get_mut(i) {
                        *slot = 0;
                    }
                    i = i.saturating_add(1);
                }
            }
            18 => {
                // Repeat zero 11..138 times.
                let repeat = 11 + br.read_bits(7)? as usize;
                for _ in 0..repeat {
                    if i >= lengths.len() {
                        return Err(InflateError::BadSymbol);
                    }
                    if let Some(slot) = lengths.get_mut(i) {
                        *slot = 0;
                    }
                    i = i.saturating_add(1);
                }
            }
            _ => return Err(InflateError::BadSymbol),
        }
    }

    // `hlit <= 286 <= lengths.len()` (the vec was built as `hlit + hdist`
    // elements), so the clamp is a no-op; `split_at` with a clamped midpoint
    // gives both halves with locally provable bounds.
    let (lit_lengths, dist_lengths) = lengths.split_at(hlit.min(lengths.len()));
    let (lit, lit_left) = Huffman::new(lit_lengths)?;
    // RFC 1951: the literal/length code must be complete (it always contains at
    // least the end-of-block symbol), so an incomplete table is malformed.
    if lit_left != 0 {
        return Err(InflateError::BadSymbol);
    }
    let (dist, dist_left) = Huffman::new(dist_lengths)?;
    // The distance code may be incomplete only in the two cases `puff.c`'s
    // dynamic() permits: an entirely empty code (no distance codes at all), or a
    // single length-1 code. Both manifest as "every used code has length 0 or
    // 1", i.e. at most one code total of length 1 (two would make it complete).
    let dist_special = dist.symbol.is_empty() || (dist.symbol.len() == 1 && dist.count[1] == 1);
    if dist_left != 0 && !dist_special {
        return Err(InflateError::BadSymbol);
    }
    Ok((lit, dist))
}

fn inflate_stored(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    max_output: usize,
) -> Result<(), InflateError> {
    br.align_to_byte();
    // Compare in `u16` and widen once afterwards: the former
    // `(len as u16) != !nlen` round-trip through `usize` left a narrowing
    // cast the verifier cannot prove lossless. Identical comparison and
    // identical `len` value.
    let len16 = br.read_aligned_u16()?;
    let nlen = br.read_aligned_u16()?;
    if len16 != !nlen {
        return Err(InflateError::BadStoredLength);
    }
    let len = len16 as usize;
    if out.len().saturating_add(len) > max_output {
        return Err(InflateError::OutputTooLarge);
    }
    out.reserve(len);
    for _ in 0..len {
        let b = br.read_aligned_byte()?;
        out.push(b);
    }
    Ok(())
}

fn inflate_block(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    max_output: usize,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), InflateError> {
    loop {
        let sym = lit.decode(br)?;
        if sym < 256 {
            if out.len() >= max_output {
                return Err(InflateError::OutputTooLarge);
            }
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(()); // end of block
        } else {
            // `sym >= 257` in this branch (`< 256` and `== 256` are handled
            // above), so the subtraction never actually saturates; the
            // saturating form keeps the bound local across the opaque
            // `decode` call.
            let li = (sym as usize).saturating_sub(257);
            // Fetch base and extra-bit count from BOTH tables through `get`:
            // the verifier cannot carry `li < 29` from one table's guard to
            // the other's raw index. Same error on the same inputs.
            let (base, extra) = match (LENGTH_BASE.get(li), LENGTH_EXTRA.get(li)) {
                (Some(&b), Some(&e)) => (b, e),
                _ => return Err(InflateError::BadSymbol),
            };
            // `extra <= 5`, so the raw bits are < 32; RFC 1951 lengths max out
            // at 258. The clamps are no-ops on every real path — they bound
            // `length` for the reservation below.
            let length = (base as usize + (br.read_bits(extra)?.min(31)) as usize).min(258);
            let dsym = dist.decode(br)? as usize;
            let (dbase, dextra) = match (DIST_BASE.get(dsym), DIST_EXTRA.get(dsym)) {
                (Some(&b), Some(&e)) => (b, e),
                _ => return Err(InflateError::BadDistance),
            };
            let distance = dbase as usize + br.read_bits(dextra)? as usize;
            // Snapshot the length ONCE: `out.reserve` below is an opaque call
            // to the verifier, after which it would no longer relate
            // `out.len()` to the `distance <= out.len()` guard.
            let cur = out.len();
            if distance == 0 || distance > cur {
                return Err(InflateError::BadDistance);
            }
            if cur.saturating_add(length) > max_output {
                return Err(InflateError::OutputTooLarge);
            }
            out.reserve(length);
            let start = cur - distance;
            if distance >= length {
                // Non-overlapping: source range [start, start+length) is fully
                // decoded (start + length = out.len() - distance + length <=
                // out.len()), so the whole match is a single memcpy. The
                // saturating end is identical on every real path (`start <=
                // cur` and `length <= 258`).
                out.extend_from_within(start..start.saturating_add(length));
            } else {
                // True RLE overlap (distance < length) must stay byte-by-byte.
                for i in 0..length {
                    // `start + i < out.len()` throughout (`out` grows each
                    // push), so the add never saturates and the element always
                    // exists; the (unreachable) `None` arm fails loudly rather
                    // than copying garbage.
                    let Some(&b) = out.get(start.saturating_add(i)) else {
                        return Err(InflateError::BadDistance);
                    };
                    out.push(b);
                }
            }
        }
    }
}

/// Decompress a raw DEFLATE (RFC 1951) stream, bounded by `max_output` bytes.
///
/// # Errors
/// Returns an [`InflateError`] for any malformed input or if the decompressed
/// size would exceed `max_output` (decompression-bomb guard). Never panics.
pub fn inflate(input: &[u8], max_output: usize) -> Result<Vec<u8>, InflateError> {
    let mut br = BitReader::new(input);
    let mut out: Vec<u8> = Vec::new();
    // The fixed-Huffman tables are pure constants, so build them at most once per
    // call and reuse across every BTYPE=1 block. A stream with no fixed block pays
    // nothing; a stream with N fixed blocks pays for the two tables exactly once
    // (an adversarial run of empty fixed blocks no longer churns allocations).
    let mut fixed: Option<(Huffman, Huffman)> = None;
    loop {
        let bfinal = br.read_bit()?;
        let btype = br.read_bits(2)?;
        match btype {
            0 => inflate_stored(&mut br, &mut out, max_output)?,
            1 => {
                // `&*` reborrows the `&mut (Huffman, Huffman)` from
                // `get_or_insert_with` as a shared ref; match ergonomics then bind
                // `lit`/`dist` as `&Huffman` for `inflate_block`'s signature.
                let (lit, dist) = &*fixed.get_or_insert_with(|| (fixed_lit(), fixed_dist()));
                inflate_block(&mut br, &mut out, max_output, lit, dist)?;
            }
            2 => {
                let (lit, dist) = read_dynamic_tables(&mut br)?;
                inflate_block(&mut br, &mut out, max_output, &lit, &dist)?;
            }
            _ => return Err(InflateError::BadBlockType),
        }
        if bfinal == 1 {
            return Ok(out);
        }
    }
}

/// The RFC 1950 Adler-32 checksum of `data`.
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        // `a` and `b` are `< MOD` at every loop head, so neither addition can
        // actually saturate; `saturating_add` keeps the no-overflow argument
        // local (the verifier drops the `< MOD` fact across iterations).
        a = a.saturating_add(u32::from(byte)) % MOD;
        b = b.saturating_add(a) % MOD;
    }
    (b << 16) | a
}

/// Decompress a zlib (RFC 1950) stream, bounded by `max_output` bytes, verifying
/// the header and the trailing Adler-32.
///
/// # Errors
/// Returns an [`InflateError`] for a bad header, malformed DEFLATE data, a size
/// over `max_output`, or an Adler-32 mismatch. Never panics.
pub fn zlib_decompress(input: &[u8], max_output: usize) -> Result<Vec<u8>, InflateError> {
    // 2-byte header + at least an empty deflate stream + 4-byte Adler-32.
    if input.len() < 6 {
        return Err(InflateError::Truncated);
    }
    let cmf = input[0];
    let flg = input[1];
    if (cmf & 0x0f) != 8 {
        return Err(InflateError::BadZlibHeader); // CM must be 8 (DEFLATE)
    }
    if ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
        return Err(InflateError::BadZlibHeader); // FCHECK
    }
    if (flg & 0x20) != 0 {
        return Err(InflateError::BadZlibHeader); // preset dictionary unsupported
    }
    let deflate = &input[2..input.len() - 4];
    let out = inflate(deflate, max_output)?;
    let trailer = &input[input.len() - 4..];
    let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    if adler32(&out) != expected {
        return Err(InflateError::BadChecksum);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference vectors produced by python3 `zlib.compress` (cross-checked).
    const EMPTY: &[u8] = &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
    const HELLO: &[u8] = &[
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x28, 0xcf, 0x2f, 0xca, 0x49, 0x01,
        0x00, 0x1d, 0x54, 0x04, 0x89,
    ];
    const STORED: &[u8] = &[
        0x78, 0x01, 0x01, 0x0a, 0x00, 0xf5, 0xff, 0x41, 0x42, 0x43, 0x44, 0x45, 0x41, 0x42, 0x43,
        0x44, 0x45, 0x0e, 0x5b, 0x02, 0x9f,
    ];
    const BACKREFS: &[u8] = &[
        0x78, 0x9c, 0x4b, 0x4c, 0x4a, 0xa4, 0x2a, 0x04, 0x00, 0xd2, 0x74, 0x1e, 0x79,
    ];
    // BTYPE=2 dynamic-Huffman block (level 9, ~1.6KB of skewed text).
    const DYNAMIC: &[u8] = &[
        0x78, 0xda, 0xed, 0xce, 0x41, 0x12, 0x02, 0x21, 0x0c, 0x44, 0xd1, 0xab, 0xf4, 0x09, 0xbc,
        0x13, 0x42, 0x18, 0xa2, 0x40, 0x30, 0x01, 0x71, 0xe6, 0xf4, 0x52, 0xde, 0xc2, 0x2a, 0x56,
        0xbd, 0xf8, 0x8b, 0x7e, 0x3d, 0x11, 0x5e, 0x83, 0xfd, 0x13, 0x77, 0x95, 0x59, 0x11, 0xe5,
        0x83, 0xc7, 0x28, 0xcd, 0x20, 0x6f, 0x52, 0xf4, 0x95, 0xb3, 0xbb, 0x4e, 0x04, 0x39, 0x30,
        0x13, 0x67, 0x82, 0x43, 0x53, 0x39, 0xd4, 0x95, 0xb2, 0xba, 0x77, 0x4a, 0x71, 0xe4, 0x7c,
        0x62, 0x2a, 0x77, 0xb2, 0x55, 0x03, 0xc5, 0xec, 0x3a, 0xad, 0xf5, 0x52, 0x9a, 0x92, 0x99,
        0x28, 0xb8, 0x42, 0x87, 0x75, 0x4c, 0xee, 0x09, 0x17, 0xa9, 0xac, 0xdc, 0xa8, 0x06, 0xaa,
        0x9e, 0xc9, 0x6e, 0xbf, 0x9f, 0xcd, 0xd8, 0x8c, 0xcd, 0xd8, 0x8c, 0xcd, 0xf8, 0x73, 0xc6,
        0x17, 0xc1, 0xd0, 0x5c, 0x27,
    ];

    #[test]
    fn empty_stream() {
        assert_eq!(zlib_decompress(EMPTY, 1 << 20).unwrap(), b"");
    }

    #[test]
    fn fixed_huffman_literals() {
        assert_eq!(zlib_decompress(HELLO, 1 << 20).unwrap(), b"hello, world");
    }

    #[test]
    fn stored_block() {
        assert_eq!(zlib_decompress(STORED, 1 << 20).unwrap(), b"ABCDEABCDE");
    }

    #[test]
    fn fixed_huffman_backreferences() {
        let expected = "ab".repeat(40).into_bytes();
        assert_eq!(zlib_decompress(BACKREFS, 1 << 20).unwrap(), expected);
    }

    #[test]
    fn dynamic_huffman_block() {
        let base = "the quick brown fox jumps over the lazy dog while a programmer \
                    carefully writes a deflate decompressor in rust with zero dependencies. ";
        let expected = base.repeat(12).into_bytes();
        assert_eq!(zlib_decompress(DYNAMIC, 1 << 20).unwrap(), expected);
    }

    #[test]
    fn raw_inflate_without_zlib_wrapper() {
        // Strip the 2-byte header + 4-byte Adler trailer -> a bare DEFLATE stream.
        let raw = &HELLO[2..HELLO.len() - 4];
        assert_eq!(inflate(raw, 1 << 20).unwrap(), b"hello, world");
    }

    #[test]
    fn output_cap_rejects_bomb() {
        // The dynamic vector expands to 1620 bytes; a 10-byte ceiling must reject it.
        assert_eq!(
            zlib_decompress(DYNAMIC, 10),
            Err(InflateError::OutputTooLarge)
        );
    }

    #[test]
    fn truncated_input_errors_not_panics() {
        for cut in 0..HELLO.len() {
            // Any prefix must return an error (or, by luck, decode) — never panic.
            let _ = zlib_decompress(&HELLO[..cut], 1 << 20);
            let _ = inflate(&HELLO[..cut], 1 << 20);
        }
    }

    #[test]
    fn bad_zlib_header_rejected() {
        assert_eq!(
            zlib_decompress(&[0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01], 1 << 20),
            Err(InflateError::BadZlibHeader)
        );
    }

    #[test]
    fn corrupt_checksum_rejected() {
        let mut bad = HELLO.to_vec();
        let n = bad.len();
        bad[n - 1] ^= 0xff; // flip the last Adler byte
        assert_eq!(
            zlib_decompress(&bad, 1 << 20),
            Err(InflateError::BadChecksum)
        );
    }

    #[test]
    fn incomplete_dynamic_huffman_rejected() {
        // Attacker-crafted zlib stream (Kitty `o=z` transport) whose BTYPE=2
        // dynamic block declares an INCOMPLETE literal/length code — {EOB: len 1,
        // 'A': len 2}, Kraft sum 1/2 + 1/4 = 3/4 < 1 — with an empty distance
        // code. Conformant zlib rejects it ("invalid literal/lengths set"), but
        // before the Kraft completeness check this decoded to Ok(b"A"): the
        // trailing Adler-32 here is adler32(b"A") = 0x00420042, so the checksum
        // gate also passed. The completeness check must now reject it.
        const INCOMPLETE: &[u8] = &[
            0x78, 0x01, 0x05, 0xc0, 0x01, 0x09, 0x00, 0x00, 0x00, 0x80, 0xa0, 0x6d, 0xfd, 0x3f,
            0x25, 0x01, 0x00, 0x42, 0x00, 0x42,
        ];
        assert_eq!(adler32(b"A"), 0x0042_0042);
        assert!(
            zlib_decompress(INCOMPLETE, 1 << 20).is_err(),
            "incomplete literal/length code must be rejected, not decoded to Ok(b\"A\")"
        );
        // Same failure on the bare DEFLATE stream (no zlib wrapper / checksum).
        let raw = &INCOMPLETE[2..INCOMPLETE.len() - 4];
        assert_eq!(inflate(raw, 1 << 20), Err(InflateError::BadSymbol));
    }

    #[test]
    fn over_subscribed_huffman_rejected() {
        // Three codes claiming length 1 over-subscribe the prefix space
        // (Kraft sum 3/2 > 1); the constructor must reject it outright.
        assert_eq!(
            Huffman::new(&[1, 1, 1]).err(),
            Some(InflateError::BadSymbol)
        );
        // A complete code (two length-1 codes) yields a zero residual.
        let (_h, left) = Huffman::new(&[1, 1]).unwrap();
        assert_eq!(left, 0);
        // An incomplete code (one length-2 code) yields a positive residual.
        let (_h, left) = Huffman::new(&[0, 2]).unwrap();
        assert!(left > 0);
    }

    #[test]
    fn adler32_known_value() {
        // Adler-32("hello, world") cross-checked against zlib.
        assert_eq!(adler32(b"hello, world"), 0x1d54_0489);
    }

    /// One step of a deterministic LCG (same constants as the engine fuzz), so any
    /// failure reproduces exactly.
    fn next(state: &mut u64) -> u32 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*state >> 33) as u32
    }

    #[test]
    fn fuzz_never_panics_and_respects_cap() {
        // Inflate parses ATTACKER bytes (the Kitty o=z transport). Throw arbitrary
        // and adversarially-mutated streams at it: it must always return Ok/Err,
        // never panic, and never produce more than `max_output` bytes.
        const CAP: usize = 4096;
        let seeds: [&[u8]; 5] = [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC];
        let mut state = 0x1234_5678_9abc_def0u64;
        for _ in 0..20_000 {
            // Build a buffer: sometimes pure noise, sometimes a mutated valid stream.
            let len = (next(&mut state) % 64) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| next(&mut state) as u8).collect();
            if next(&mut state) & 1 == 0 {
                // Mutate a real vector: truncate and flip some bytes.
                let seed = seeds[(next(&mut state) as usize) % seeds.len()];
                let cut = (next(&mut state) as usize) % (seed.len() + 1);
                buf = seed[..cut].to_vec();
                for _ in 0..(next(&mut state) % 4) {
                    if !buf.is_empty() {
                        let idx = (next(&mut state) as usize) % buf.len();
                        buf[idx] ^= (next(&mut state) as u8) | 1;
                    }
                }
            }
            if let Ok(out) = zlib_decompress(&buf, CAP) {
                assert!(out.len() <= CAP, "zlib_decompress exceeded the cap");
            }
            if let Ok(out) = inflate(&buf, CAP) {
                assert!(out.len() <= CAP, "inflate exceeded the cap");
            }
        }
    }

    // ---------------------------------------------------------------------
    // Differential safety net for the buffered BitReader (finding 5).
    //
    // A faithful copy of the ORIGINAL per-bit reader and decode path. The only
    // intended difference from the production decoder is the bit-IO core (and the
    // back-reference memcpy), so decoding the reference vectors AND every
    // truncation prefix through both must produce byte-identical output and the
    // identical `InflateError`. Any drift in the truncation boundary or the bit
    // ordering of the buffered reader fails here.
    // ---------------------------------------------------------------------

    /// The original LSB-first per-bit reader, kept verbatim as a reference.
    struct RefBitReader<'a> {
        data: &'a [u8],
        byte_pos: usize,
        bit_pos: u32,
    }

    impl<'a> RefBitReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self {
                data,
                byte_pos: 0,
                bit_pos: 0,
            }
        }

        fn read_bit(&mut self) -> Result<u32, InflateError> {
            let byte = *self
                .data
                .get(self.byte_pos)
                .ok_or(InflateError::Truncated)?;
            let bit = (byte >> self.bit_pos) & 1;
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
            Ok(u32::from(bit))
        }

        fn read_bits(&mut self, n: u32) -> Result<u32, InflateError> {
            let mut v = 0u32;
            for i in 0..n {
                v |= self.read_bit()? << i;
            }
            Ok(v)
        }

        fn align_to_byte(&mut self) {
            if self.bit_pos != 0 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }

        fn read_aligned_byte(&mut self) -> Result<u8, InflateError> {
            let b = *self
                .data
                .get(self.byte_pos)
                .ok_or(InflateError::Truncated)?;
            self.byte_pos += 1;
            Ok(b)
        }

        fn read_aligned_u16(&mut self) -> Result<u16, InflateError> {
            let lo = self.read_aligned_byte()?;
            let hi = self.read_aligned_byte()?;
            Ok(u16::from_le_bytes([lo, hi]))
        }
    }

    // The reference reuses the production `Huffman` table construction (unchanged)
    // and only re-implements the bit-consuming decode against `RefBitReader`.
    fn ref_decode(h: &Huffman, br: &mut RefBitReader) -> Result<u16, InflateError> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=MAXBITS {
            code |= br.read_bit()? as i32;
            let cnt = i32::from(h.count[len]);
            if code - first < cnt {
                let idx = (index + (code - first)) as usize;
                return h.symbol.get(idx).copied().ok_or(InflateError::BadSymbol);
            }
            index += cnt;
            first += cnt;
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::BadSymbol)
    }

    fn ref_read_dynamic_tables(br: &mut RefBitReader) -> Result<(Huffman, Huffman), InflateError> {
        let hlit = br.read_bits(5)? as usize + 257;
        let hdist = br.read_bits(5)? as usize + 1;
        let hclen = br.read_bits(4)? as usize + 4;
        if hlit > 286 || hdist > 30 || hclen > 19 {
            return Err(InflateError::BadSymbol);
        }
        let mut cl_lengths = [0u16; 19];
        for &slot in CLCL_ORDER.iter().take(hclen) {
            cl_lengths[slot] = br.read_bits(3)? as u16;
        }
        let (cl_huff, cl_left) = Huffman::new(&cl_lengths)?;
        if cl_left != 0 {
            return Err(InflateError::BadSymbol);
        }

        let mut lengths = vec![0u16; hlit + hdist];
        let mut i = 0;
        while i < lengths.len() {
            let sym = ref_decode(&cl_huff, br)?;
            match sym {
                0..=15 => {
                    lengths[i] = sym;
                    i += 1;
                }
                16 => {
                    if i == 0 {
                        return Err(InflateError::BadSymbol);
                    }
                    let prev = lengths[i - 1];
                    let repeat = 3 + br.read_bits(2)? as usize;
                    for _ in 0..repeat {
                        if i >= lengths.len() {
                            return Err(InflateError::BadSymbol);
                        }
                        lengths[i] = prev;
                        i += 1;
                    }
                }
                17 => {
                    let repeat = 3 + br.read_bits(3)? as usize;
                    for _ in 0..repeat {
                        if i >= lengths.len() {
                            return Err(InflateError::BadSymbol);
                        }
                        lengths[i] = 0;
                        i += 1;
                    }
                }
                18 => {
                    let repeat = 11 + br.read_bits(7)? as usize;
                    for _ in 0..repeat {
                        if i >= lengths.len() {
                            return Err(InflateError::BadSymbol);
                        }
                        lengths[i] = 0;
                        i += 1;
                    }
                }
                _ => return Err(InflateError::BadSymbol),
            }
        }
        let (lit, lit_left) = Huffman::new(&lengths[..hlit])?;
        if lit_left != 0 {
            return Err(InflateError::BadSymbol);
        }
        let (dist, dist_left) = Huffman::new(&lengths[hlit..])?;
        let dist_special = dist.symbol.is_empty() || (dist.symbol.len() == 1 && dist.count[1] == 1);
        if dist_left != 0 && !dist_special {
            return Err(InflateError::BadSymbol);
        }
        Ok((lit, dist))
    }

    fn ref_inflate_stored(
        br: &mut RefBitReader,
        out: &mut Vec<u8>,
        max_output: usize,
    ) -> Result<(), InflateError> {
        br.align_to_byte();
        let len = br.read_aligned_u16()? as usize;
        let nlen = br.read_aligned_u16()?;
        if (len as u16) != !nlen {
            return Err(InflateError::BadStoredLength);
        }
        if out.len().saturating_add(len) > max_output {
            return Err(InflateError::OutputTooLarge);
        }
        out.reserve(len);
        for _ in 0..len {
            let b = br.read_aligned_byte()?;
            out.push(b);
        }
        Ok(())
    }

    fn ref_inflate_block(
        br: &mut RefBitReader,
        out: &mut Vec<u8>,
        max_output: usize,
        lit: &Huffman,
        dist: &Huffman,
    ) -> Result<(), InflateError> {
        loop {
            let sym = ref_decode(lit, br)?;
            if sym < 256 {
                if out.len() >= max_output {
                    return Err(InflateError::OutputTooLarge);
                }
                out.push(sym as u8);
            } else if sym == 256 {
                return Ok(());
            } else {
                let li = sym as usize - 257;
                let length = *LENGTH_BASE.get(li).ok_or(InflateError::BadSymbol)? as usize
                    + br.read_bits(LENGTH_EXTRA[li])? as usize;
                let dsym = ref_decode(dist, br)? as usize;
                let distance = *DIST_BASE.get(dsym).ok_or(InflateError::BadDistance)? as usize
                    + br.read_bits(DIST_EXTRA[dsym])? as usize;
                if distance == 0 || distance > out.len() {
                    return Err(InflateError::BadDistance);
                }
                if out.len().saturating_add(length) > max_output {
                    return Err(InflateError::OutputTooLarge);
                }
                out.reserve(length);
                let start = out.len() - distance;
                for i in 0..length {
                    let b = out[start + i];
                    out.push(b);
                }
            }
        }
    }

    fn ref_inflate(input: &[u8], max_output: usize) -> Result<Vec<u8>, InflateError> {
        let mut br = RefBitReader::new(input);
        let mut out: Vec<u8> = Vec::new();
        loop {
            let bfinal = br.read_bit()?;
            let btype = br.read_bits(2)?;
            match btype {
                0 => ref_inflate_stored(&mut br, &mut out, max_output)?,
                1 => ref_inflate_block(&mut br, &mut out, max_output, &fixed_lit(), &fixed_dist())?,
                2 => {
                    let (lit, dist) = ref_read_dynamic_tables(&mut br)?;
                    ref_inflate_block(&mut br, &mut out, max_output, &lit, &dist)?;
                }
                _ => return Err(InflateError::BadBlockType),
            }
            if bfinal == 1 {
                return Ok(out);
            }
        }
    }

    fn ref_zlib_decompress(input: &[u8], max_output: usize) -> Result<Vec<u8>, InflateError> {
        if input.len() < 6 {
            return Err(InflateError::Truncated);
        }
        let cmf = input[0];
        let flg = input[1];
        if (cmf & 0x0f) != 8 {
            return Err(InflateError::BadZlibHeader);
        }
        if ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
            return Err(InflateError::BadZlibHeader);
        }
        if (flg & 0x20) != 0 {
            return Err(InflateError::BadZlibHeader);
        }
        let deflate = &input[2..input.len() - 4];
        let out = ref_inflate(deflate, max_output)?;
        let trailer = &input[input.len() - 4..];
        let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
        if adler32(&out) != expected {
            return Err(InflateError::BadChecksum);
        }
        Ok(out)
    }

    #[test]
    fn buffered_reader_matches_per_bit_reader_on_vectors_and_truncations() {
        const CAP: usize = 1 << 20;
        let vectors: [&[u8]; 5] = [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC];
        for v in vectors {
            // Every truncation prefix (including the full stream at cut == len).
            for cut in 0..=v.len() {
                let prefix = &v[..cut];
                assert_eq!(
                    inflate(prefix, CAP),
                    ref_inflate(prefix, CAP),
                    "raw inflate disagreed at cut {cut} of a {}-byte vector",
                    v.len()
                );
                assert_eq!(
                    zlib_decompress(prefix, CAP),
                    ref_zlib_decompress(prefix, CAP),
                    "zlib_decompress disagreed at cut {cut} of a {}-byte vector",
                    v.len()
                );
            }
        }
    }
}
