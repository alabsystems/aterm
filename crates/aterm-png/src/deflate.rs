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
mod tests {
    use super::*;

    /// Round-trip through aterm's OWN inflate — the decoder this crate's PNG
    /// reader uses, so a stream that survives this is a stream aterm can read.
    fn roundtrip(data: &[u8]) {
        let z = zlib_compress(data);
        let back = aterm_codec::inflate::zlib_decompress(&z, data.len() + 64)
            .unwrap_or_else(|e| panic!("inflate failed on {} bytes: {e:?}", data.len()));
        assert_eq!(back, data, "round-trip differed at {} bytes", data.len());
    }

    #[test]
    fn roundtrips_edge_shapes() {
        roundtrip(b"");
        roundtrip(b"a");
        roundtrip(b"ab");
        roundtrip(b"abc");
        roundtrip(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        roundtrip(&[0u8; 1024]);
        roundtrip(&[0xFFu8; 300_000]);
        roundtrip(b"the quick brown fox jumps over the lazy dog");
        let all: Vec<u8> = (0..=255u8).collect();
        roundtrip(&all);
        let repeated: Vec<u8> = all.iter().cycle().take(70_000).copied().collect();
        roundtrip(&repeated);
    }

    /// A maximal match (258 bytes) and a maximal distance (32768) must both be
    /// expressible — the two ends of the code tables.
    #[test]
    fn roundtrips_extreme_match_geometry() {
        let mut max_len = vec![7u8; 258];
        max_len.extend(std::iter::repeat_n(0u8, WINDOW - 258));
        max_len.extend(std::iter::repeat_n(7u8, 258));
        roundtrip(&max_len);

        // Place `[9, 8, 7]` only at index 0 and index `WINDOW` (32768) so the greedy
        // matcher has no nearer `[9, 8, 7]` candidate and emits distance 32768 (code 29,
        // all 13 extra bits set).
        let mut max_dist = vec![9u8, 8, 7];
        max_dist.extend(std::iter::repeat_n(0u8, WINDOW - 3));
        max_dist.extend_from_slice(&[9, 8, 7]);
        roundtrip(&max_dist);
    }

    /// Pseudo-random bytes are incompressible; the stream must still be valid
    /// (this is where a fixed block is at its worst and the bit packing is most
    /// likely to be caught out).
    #[test]
    fn roundtrips_incompressible_noise() {
        let mut state = 0x1234_5678u32;
        let noise: Vec<u8> = (0..50_000)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect();
        roundtrip(&noise);
    }

    /// Structured, image-like input across two shapes that stress different
    /// halves of the matcher:
    ///
    /// * a raw RGBA gradient, whose per-pixel bytes all change;
    /// * a PNG-filtered residual stream with long runs of small residuals.
    #[test]
    fn roundtrips_image_like_input() {
        let mut raw = Vec::new();
        for y in 0..200u32 {
            raw.push(0); // filter byte
            for x in 0..200u32 {
                raw.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128, 255]);
            }
        }
        roundtrip(&raw);

        let mut filtered = Vec::new();
        for _ in 0..200 {
            filtered.push(1u8); // Sub filter
            filtered.extend(std::iter::repeat_n(0u8, 400));
            filtered.extend((0..400u32).map(|i| ((i * 7) % 3) as u8));
        }
        roundtrip(&filtered);
    }

    #[test]
    fn zlib_header_is_well_formed() {
        let z = zlib_compress(b"hello");
        assert_eq!(z[0] & 0x0F, 8, "CM must be deflate");
        assert_eq!(z[0] >> 4, 7, "CINFO must be the 32 KiB window");
        assert_eq!(
            (u16::from(z[0]) << 8 | u16::from(z[1])) % 31,
            0,
            "FCHECK must make the header pair a multiple of 31"
        );
        assert_eq!(z[1] & 0x20, 0, "no preset dictionary");
        let n = z.len();
        assert_eq!(
            u32::from_be_bytes([z[n - 4], z[n - 3], z[n - 2], z[n - 1]]),
            super::super::checksum::adler32(b"hello"),
        );
    }

    /// Reference whole-buffer compressor with one `prev` link per input position.
    fn whole_buffer_zlib(data: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::with_capacity(0);
        w.bits(1, 1);
        w.bits(1, 2);
        let mut head = vec![usize::MAX; HASH_SIZE];
        let mut prev = vec![usize::MAX; data.len()];
        let mut i = 0usize;
        while i < data.len() {
            let mut best_len = 0usize;
            let mut best_dist = 0usize;
            if i + MIN_MATCH <= data.len() {
                let h = hash3(data, i);
                let mut candidate = head[h];
                let mut chain = 0usize;
                let limit = (data.len() - i).min(MAX_MATCH);
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
                    if best_len >= MIN_MATCH
                        && i + best_len < data.len()
                        && data[candidate + best_len] != data[i + best_len]
                    {
                        candidate = prev[candidate];
                        continue;
                    }
                    let mut len = 0usize;
                    while len < limit && data[candidate + len] == data[i + len] {
                        len += 1;
                    }
                    if len > best_len {
                        best_len = len;
                        best_dist = dist;
                    }
                    candidate = prev[candidate];
                }
            }
            if best_len >= MIN_MATCH {
                let (code, extra, extra_bits) = length_code(best_len as u16);
                let (lit_code, lit_len) = fixed_litlen(code);
                w.huffman(lit_code, lit_len);
                if extra_bits > 0 {
                    w.bits(extra, extra_bits);
                }
                let (dcode, dextra, dextra_bits) = distance_code(best_dist as u16);
                w.huffman(dcode, 5);
                if dextra_bits > 0 {
                    w.bits(dextra, dextra_bits);
                }
                for (k, slot) in prev.iter_mut().enumerate().skip(i).take(best_len) {
                    if k + MIN_MATCH <= data.len() {
                        let h = hash3(data, k);
                        *slot = head[h];
                        head[h] = k;
                    }
                }
                i += best_len;
            } else {
                let (code, len) = fixed_litlen(u16::from(data[i]));
                w.huffman(code, len);
                if i + MIN_MATCH <= data.len() {
                    let h = hash3(data, i);
                    prev[i] = head[h];
                    head[h] = i;
                }
                i += 1;
            }
        }
        let (code, len) = fixed_litlen(256);
        w.huffman(code, len);
        let mut out = vec![0x78, 0x01];
        out.extend_from_slice(&w.finish());
        out.extend_from_slice(&super::super::checksum::adler32(data).to_be_bytes());
        out
    }

    /// Seeded inputs that reach every part of the window logic: noise,
    /// filtered-scanline shapes, runs far longer than the window, and repeats
    /// at distances on both sides of it.
    fn window_cases() -> Vec<(&'static str, Vec<u8>)> {
        let mut state = 0x2545_F491u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        let noise: Vec<u8> = (0..3 * WINDOW + 777)
            .map(|_| (next() >> 24) as u8)
            .collect();
        let mut scanlines = Vec::new();
        for y in 0..400u32 {
            scanlines.push((y % 5) as u8);
            scanlines.extend((0..1024u32).map(|x| ((x * 3 + y) % 7) as u8));
        }
        let run = vec![0xA5u8; 5 * WINDOW + 13];
        let block: Vec<u8> = (0..300).map(|_| (next() >> 24) as u8).collect();
        let mut straddle = block.clone();
        straddle.extend((0..WINDOW - block.len()).map(|_| (next() >> 24) as u8));
        straddle.extend_from_slice(&block);
        straddle.extend((0..WINDOW - block.len() + 1).map(|_| (next() >> 24) as u8));
        straddle.extend_from_slice(&block);
        let mut mixed = Vec::new();
        for i in 0..40u32 {
            let len = (next() % 9000) as usize;
            if i % 3 == 0 {
                mixed.extend(std::iter::repeat_n((next() >> 24) as u8, len));
            } else {
                mixed.extend((0..len).map(|_| (next() >> 28) as u8));
            }
        }
        vec![
            ("empty", Vec::new()),
            ("short", b"abcabcabcabd".to_vec()),
            ("noise", noise),
            ("scanlines", scanlines),
            ("run", run),
            ("straddle", straddle),
            ("mixed", mixed),
        ]
    }

    #[test]
    fn streaming_output_is_byte_identical_to_the_whole_buffer_compressor() {
        let mut state = 0x9E37_79B9u32;
        for (label, data) in window_cases() {
            let oracle = whole_buffer_zlib(&data);
            assert_eq!(zlib_compress(&data), oracle, "{label}: one write");

            let mut rows = ZlibStream::new();
            for row in data.chunks(1025) {
                rows.write(row);
            }
            assert_eq!(rows.finish(), oracle, "{label}: scanline writes");

            let mut random = ZlibStream::new();
            let mut rest = &data[..];
            while !rest.is_empty() {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let take = (state as usize % (2 * WINDOW)).min(rest.len());
                random.write(&rest[..take]);
                rest = &rest[take..];
            }
            assert_eq!(random.finish(), oracle, "{label}: random splits");

            if data.len() <= 3 * WINDOW + 1000 {
                let mut bytes = ZlibStream::new();
                for byte in &data {
                    bytes.write(std::slice::from_ref(byte));
                }
                assert_eq!(bytes.finish(), oracle, "{label}: byte writes");
            }
        }
    }

    /// The compressor's memory is bounded by [`STATE_BYTES`], not by its input:
    /// a 1 MiB write and a scanline-at-a-time stream leave the window at the
    /// capacity it was born with, and the chains at one window.
    #[test]
    fn the_window_never_outgrows_its_capacity() {
        let data: Vec<u8> = (0..(1usize << 20)).map(|i| (i * 31 % 251) as u8).collect();
        for split in [data.len(), 4097] {
            let mut stream = ZlibStream::new();
            for piece in data.chunks(split) {
                stream.write(piece);
                assert_eq!(stream.window.capacity(), WINDOW_CAPACITY, "split {split}");
                assert!(stream.window.len() <= WINDOW_CAPACITY, "split {split}");
            }
            assert_eq!((stream.head.len(), stream.prev.len()), (HASH_SIZE, WINDOW));
            assert_eq!(stream.finish(), whole_buffer_zlib(&data), "split {split}");
        }
    }
}
