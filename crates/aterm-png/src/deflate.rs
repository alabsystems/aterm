// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Streaming zlib compressor for the PNG encoder ([`STATE_BYTES`]).

use crate::checksum::Adler32;

/// LSB-first bit sink for DEFLATE ([`BitWriter::huffman`], [`BitWriter::bits`]).
pub(crate) struct BitWriter {
    out: Vec<u8>,
    /// Bits not yet flushed, LSB-first: bit 0 is the next bit to emit.
    acc: u32,
    /// How many low bits of `acc` are live.
    n: u32,
}

impl BitWriter {
    #[inline]
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            out: Vec::with_capacity(capacity),
            acc: 0,
            n: 0,
        }
    }

    /// Emit `count` bits of `value`, least-significant bit first.
    #[inline]
    pub(crate) fn bits(&mut self, value: u32, count: u32) {
        debug_assert!(count <= 24, "bit runs stay inside the accumulator");
        self.acc |= (value & ((1u32 << count) - 1)) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Emit a Huffman code: the same bits, but most-significant FIRST.
    #[inline]
    pub(crate) fn huffman(&mut self, code: u16, len: u32) {
        let mut reversed = 0u32;
        for i in 0..len {
            reversed |= ((u32::from(code) >> i) & 1) << (len - 1 - i);
        }
        self.bits(reversed, len);
    }

    /// Pad the partial byte with zeroes, so the next bit starts a byte (a stored block's
    /// `LEN`).
    #[inline]
    pub(crate) fn align(&mut self) {
        if self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc = 0;
            self.n = 0;
        }
    }

    /// Append whole bytes; the writer must be [`BitWriter::align`]ed.
    #[inline]
    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        debug_assert_eq!(self.n, 0, "bytes go out on a byte boundary");
        self.out.extend_from_slice(bytes);
    }

    /// How many bits are pending past the last whole byte.
    #[inline]
    pub(crate) const fn pending(&self) -> u32 {
        self.n
    }

    /// Pad the final partial byte with zeroes and take the buffer.
    #[inline]
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// Smallest match length code 257..285 stands for.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits carried after each length code.
pub(crate) const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Smallest distance each distance code 0..29 stands for.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits carried after each distance code.
pub(crate) const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// The fixed literal/length code for `symbol` (`0..=287`) as `(code, bit length)`.
#[inline]
pub(crate) const fn fixed_litlen(symbol: u16) -> (u16, u32) {
    debug_assert!(symbol <= 287);
    match symbol {
        0..=143 => (0x30 + symbol, 8),
        144..=255 => (0x190 + (symbol - 144), 9),
        256..=279 => (symbol - 256, 7),
        _ => (0xC0 + (symbol - 280), 8),
    }
}

/// The length code (257..=285) and extra-bit payload for a match length.
#[inline]
pub(crate) const fn length_code(len: u16) -> (u16, u32, u32) {
    debug_assert!(len >= 3 && len <= 258);
    let mut i = LENGTH_BASE.len() - 1;
    while i > 0 && LENGTH_BASE[i] > len {
        i -= 1;
    }
    (
        257 + i as u16,
        (len - LENGTH_BASE[i]) as u32,
        LENGTH_EXTRA[i],
    )
}

/// The distance code (0..=29) and extra-bit payload for a match distance.
#[inline]
pub(crate) const fn distance_code(dist: u16) -> (u16, u32, u32) {
    debug_assert!(dist >= 1 && dist <= 32768);
    let mut i = DIST_BASE.len() - 1;
    while i > 0 && DIST_BASE[i] > dist {
        i -= 1;
    }
    (i as u16, (dist - DIST_BASE[i]) as u32, DIST_EXTRA[i])
}

/// DEFLATE's window: a match may reach at most this far back.
pub(crate) const WINDOW: usize = 32768;
const _: () = assert!(WINDOW.is_power_of_two());
/// DEFLATE's longest expressible match.
pub(crate) const MAX_MATCH: usize = 258;
/// Shortest match worth emitting.
pub(crate) const MIN_MATCH: usize = 3;
/// Hash table size (a power of two, so the mask is the modulus).
const HASH_BITS: usize = 15;
pub(crate) const HASH_SIZE: usize = 1 << HASH_BITS;
/// How many candidates a single position will try before settling.
const MAX_CHAIN: usize = 32;
/// A match at least this long ends the search immediately.
const GOOD_ENOUGH: usize = 32;

/// Hash the three bytes at `data[i..]` into a bucket index.
#[inline]
pub(crate) const fn hash3(data: &[u8], i: usize) -> usize {
    let h = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i + 2] as u32);
    ((h.wrapping_mul(2_654_435_761)) >> (32 - HASH_BITS)) as usize & (HASH_SIZE - 1)
}

/// Bytes past a position the matcher must hold before it decides that position.
pub(crate) const LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH;

/// The most input [`ZlibStream::write`] appends to its window per matcher pass.
const PIECE: usize = WINDOW;

/// Window capacity: `2 * `[`WINDOW`], one [`PIECE`] of input, and [`LOOKAHEAD`].
const WINDOW_CAPACITY: usize = 2 * WINDOW + PIECE + LOOKAHEAD;

/// The bytes a [`ZlibStream`] holds beside its pending output, whatever its input length.
pub(crate) const STATE_BYTES: usize =
    (HASH_SIZE + WINDOW) * std::mem::size_of::<usize>() + WINDOW_CAPACITY;

/// Streaming RFC 1950 zlib compressor ([`WINDOW`], [`LOOKAHEAD`], [`ZlibStream::write`]).
pub(crate) struct ZlibStream {
    bits: BitWriter,
    /// `head[h]` = most recent position hashing to `h`.
    head: Vec<usize>,
    /// `prev[i & (WINDOW - 1)]` = previous position in `i`'s bucket within [`WINDOW`].
    prev: Vec<usize>,
    /// Input from absolute position `base` on.
    window: Vec<u8>,
    base: usize,
    /// The next position to decide.
    pos: usize,
    adler: Adler32,
}

impl ZlibStream {
    /// An empty stream with no output reserved.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_output_capacity(0)
    }

    /// Empty stream with `capacity` output bytes preallocated ([`ZlibStream::output`]).
    pub(crate) fn with_output_capacity(capacity: usize) -> Self {
        let mut bits = BitWriter::with_capacity(capacity);
        bits.out.extend_from_slice(&[0x78, 0x01]);
        bits.bits(1, 1);
        bits.bits(1, 2);
        Self {
            bits,
            head: vec![usize::MAX; HASH_SIZE],
            prev: vec![usize::MAX; WINDOW],
            window: Vec::with_capacity(WINDOW_CAPACITY),
            base: 0,
            pos: 0,
            adler: Adler32::new(),
        }
    }

    /// Compress `data`, continuing the stream ([`LOOKAHEAD`], [`ZlibStream::finish`]).
    pub(crate) fn write(&mut self, data: &[u8]) {
        self.adler.update(data);
        for piece in data.chunks(PIECE) {
            self.window.extend_from_slice(piece);
            self.compress(false);
            let reach = self.pos.saturating_sub(WINDOW);
            if reach - self.base >= WINDOW {
                self.window.drain(..reach - self.base);
                self.base = reach;
            }
        }
    }

    /// The compressed bytes produced so far and not yet taken.
    #[inline]
    pub(crate) const fn output(&mut self) -> &mut Vec<u8> {
        &mut self.bits.out
    }

    /// Compress the rest, close the block and append the Adler-32.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.compress(true);
        let (code, len) = fixed_litlen(256);
        self.bits.huffman(code, len);
        let adler = self.adler.finish();
        let mut out = self.bits.finish();
        out.extend_from_slice(&adler.to_be_bytes());
        out
    }

    /// Decide every position that has [`LOOKAHEAD`] input past it.
    fn compress(&mut self, last: bool) {
        let Self {
            bits,
            head,
            prev,
            window,
            base,
            pos,
            ..
        } = self;
        let base = *base;
        let data = &window[..];
        let end = base + data.len();
        let mut i = *pos;
        while i < end && (last || end - i >= LOOKAHEAD) {
            let mut best_len = 0usize;
            let mut best_dist = 0usize;

            if i + MIN_MATCH <= end {
                let h = hash3(data, i - base);
                let mut candidate = head[h];
                let mut chain = 0usize;
                let limit = (end - i).min(MAX_MATCH);
                while candidate != usize::MAX
                    && chain < MAX_CHAIN
                    && best_len < GOOD_ENOUGH
                    && best_len < limit
                {
                    chain += 1;
                    let dist = i - candidate;
                    if dist > WINDOW {
                        break;
                    }
                    let (c, r) = (candidate - base, i - base);
                    if best_len >= MIN_MATCH
                        && i + best_len < end
                        && data[c + best_len] != data[r + best_len]
                    {
                        candidate = prev[candidate & (WINDOW - 1)];
                        continue;
                    }
                    let mut len = 0usize;
                    while len < limit && data[c + len] == data[r + len] {
                        len += 1;
                    }
                    if len > best_len {
                        best_len = len;
                        best_dist = dist;
                    }
                    candidate = prev[candidate & (WINDOW - 1)];
                }
            }

            if best_len >= MIN_MATCH {
                let (code, extra, extra_bits) = length_code(best_len as u16);
                let (lit_code, lit_len) = fixed_litlen(code);
                bits.huffman(lit_code, lit_len);
                if extra_bits > 0 {
                    bits.bits(extra, extra_bits);
                }
                let (dcode, dextra, dextra_bits) = distance_code(best_dist as u16);
                bits.huffman(dcode, 5);
                if dextra_bits > 0 {
                    bits.bits(dextra, dextra_bits);
                }
                for k in i..i + best_len {
                    if k + MIN_MATCH <= end {
                        let h = hash3(data, k - base);
                        prev[k & (WINDOW - 1)] = head[h];
                        head[h] = k;
                    }
                }
                i += best_len;
            } else {
                let (code, len) = fixed_litlen(u16::from(data[i - base]));
                bits.huffman(code, len);
                if i + MIN_MATCH <= end {
                    let h = hash3(data, i - base);
                    prev[i & (WINDOW - 1)] = head[h];
                    head[h] = i;
                }
                i += 1;
            }
        }
        *pos = i;
    }
}

/// `data` as one complete zlib stream.
#[cfg(test)]
pub(crate) fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut stream = ZlibStream::new();
    stream.write(data);
    stream.finish()
}

#[cfg(test)]
#[path = "deflate_tests.rs"]
mod tests;
