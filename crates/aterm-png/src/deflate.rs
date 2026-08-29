// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The zlib COMPRESSOR the PNG encoder needs (RFC 1950 + RFC 1951).
//!
//! aterm already had the other half: `aterm_codec::inflate` decompresses, and
//! the decoder in this crate uses it. Nothing in the tree could WRITE a deflate
//! stream, which is the only reason `png` (and through it `flate2`,
//! `miniz_oxide`, `fdeflate`, `simd-adler32`, `crc32fast` and `adler2`) was
//! still linked.
//!
//! ## What this is, and what it deliberately is not
//!
//! Fixed-Huffman blocks (RFC 1951 §3.2.6) over a greedy LZ77 match finder with a
//! 32 KiB window and hash-chained candidates. No dynamic Huffman tables, no
//! block-splitting heuristics, no lazy matching.
//!
//! A dynamic-Huffman encoder would beat this on entropy. The trade was accepted
//! on the expectation of losing some ratio for a large reduction in surface —
//! and then MEASURED, because an expectation is not a number.
//!
//! `tests/oracle.rs::compression_ratio_against_the_oracle_is_recorded` re-encodes
//! 60 images from the repository's own corpus through both stacks. Result at the
//! time of writing: 31,157,948 raw bytes become 3,304,492 through this encoder
//! and 3,876,652 through `png` — this one is **0.85x the size**, i.e. slightly
//! SMALLER. The reason is not that fixed Huffman beats dynamic Huffman; it is
//! that `png`'s default compression setting is `Fast`, and the per-row adaptive
//! filter selection in `encode::filter_image` matters more for image data than
//! the entropy stage does. The test keeps that number honest, and its assertion
//! is deliberately loose (fail only past 3x) because it guards against a
//! compressor that has stopped compressing, not against a few percent of drift.
//!
//! Every stream this emits is a VALID zlib stream, so `png`, `flate2`, `zlib`
//! and every browser read it; that equality is what the differential test pins.

/// LSB-first bit sink, the bit order DEFLATE packs into bytes.
///
/// Huffman codes are packed MSB-first WITHIN this LSB-first stream (RFC 1951
/// §3.1.1) — the one place the two orders meet, and the classic bug — so codes
/// go through [`BitWriter::huffman`] and everything else through
/// [`BitWriter::bits`].
struct BitWriter {
    out: Vec<u8>,
    /// Bits not yet flushed, LSB-first: bit 0 is the next bit to emit.
    acc: u32,
    /// How many low bits of `acc` are live.
    n: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            acc: 0,
            n: 0,
        }
    }

    /// Emit `count` bits of `value`, least-significant bit first.
    fn bits(&mut self, value: u32, count: u32) {
        debug_assert!(count <= 24, "bit runs stay inside the accumulator");
        self.acc |= (value & ((1u32 << count) - 1)) << self.n;
        self.n += count;
        while self.n >= 8 {
            // `as u8` is the intended truncation to the low byte.
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Emit a Huffman code: the same bits, but most-significant FIRST.
    fn huffman(&mut self, code: u16, len: u32) {
        let mut reversed = 0u32;
        for i in 0..len {
            reversed |= ((u32::from(code) >> i) & 1) << (len - 1 - i);
        }
        self.bits(reversed, len);
    }

    /// Pad the final partial byte with zeroes and take the buffer.
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            // `as u8`: only the live low bits remain.
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

// ---------------------------------------------------------------------------
// RFC 1951 §3.2.5 code tables, transcribed as (base, extra-bits) pairs.
// ---------------------------------------------------------------------------

/// Smallest match length code 257..285 stands for.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits carried after each length code.
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Smallest distance each distance code 0..29 stands for.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits carried after each distance code.
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// The fixed literal/length code for `symbol` as `(code, bit length)`.
///
/// RFC 1951 §3.2.6's table, as the four ranges it is actually defined by rather
/// than 288 transcribed literals.
fn fixed_litlen(symbol: u16) -> (u16, u32) {
    match symbol {
        0..=143 => (0x30 + symbol, 8),
        144..=255 => (0x190 + (symbol - 144), 9),
        256..=279 => (symbol - 256, 7),
        // 280..=287, and anything past the alphabet cannot be produced here.
        _ => (0xC0 + (symbol - 280), 8),
    }
}

/// The length code (257..=285) and extra-bit payload for a match length.
fn length_code(len: u16) -> (u16, u32, u32) {
    debug_assert!((3..=258).contains(&len));
    let mut i = LENGTH_BASE.len() - 1;
    while i > 0 && LENGTH_BASE[i] > len {
        i -= 1;
    }
    // `as u16`/`as u32`: i < 29, and len - base fits the code's extra bits.
    (
        257 + i as u16,
        u32::from(len - LENGTH_BASE[i]),
        LENGTH_EXTRA[i],
    )
}

/// The distance code (0..=29) and extra-bit payload for a match distance.
fn distance_code(dist: u16) -> (u16, u32, u32) {
    debug_assert!((1..=32768).contains(&u32::from(dist).max(1)));
    let mut i = DIST_BASE.len() - 1;
    while i > 0 && DIST_BASE[i] > dist {
        i -= 1;
    }
    (i as u16, u32::from(dist - DIST_BASE[i]), DIST_EXTRA[i])
}

// ---------------------------------------------------------------------------
// LZ77
// ---------------------------------------------------------------------------

/// DEFLATE's window: a match may reach at most this far back.
const WINDOW: usize = 32768;
/// DEFLATE's longest expressible match.
const MAX_MATCH: usize = 258;
/// Shortest match worth emitting — below this a literal pair is cheaper, and
/// the hash is over exactly this many bytes.
const MIN_MATCH: usize = 3;
/// Hash table size (a power of two, so the mask is the modulus).
const HASH_BITS: usize = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;
/// How many candidates a single position will try before settling. Bounds the
/// worst case (a long run of one byte hashes every position to one bucket) so
/// compression time stays linear-ish in the input rather than quadratic.
const MAX_CHAIN: usize = 32;
/// A match at least this long ends the search immediately.
const GOOD_ENOUGH: usize = 32;

/// Hash the three bytes at `data[i..]` into a bucket index.
fn hash3(data: &[u8], i: usize) -> usize {
    let h = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8) | u32::from(data[i + 2]);
    // Knuth multiplicative; take the high HASH_BITS of the product.
    ((h.wrapping_mul(2_654_435_761)) >> (32 - HASH_BITS)) as usize & (HASH_SIZE - 1)
}

/// Compress `data` into ONE fixed-Huffman DEFLATE block (raw, no zlib wrapper).
fn deflate_fixed(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::new();
    // BFINAL = 1, BTYPE = 01 (fixed Huffman).
    w.bits(1, 1);
    w.bits(1, 2);

    // `head[h]` = most recent position hashing to `h`; `prev[i]` = the position
    // before `i` in the same bucket. `usize::MAX` is the empty sentinel, which
    // no real position can be.
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
            // How far a match may run: to end-of-input, capped at MAX_MATCH.
            let limit = (data.len() - i).min(MAX_MATCH);
            while candidate != usize::MAX
                && chain < MAX_CHAIN
                && best_len < GOOD_ENOUGH
                // Nothing can beat a match that already reaches the search
                // limit (end of input, or DEFLATE's 258-byte ceiling).
                && best_len < limit
            {
                chain += 1;
                let dist = i - candidate;
                if dist > WINDOW {
                    break;
                }
                // Cheap reject before the byte-by-byte walk: a candidate can only
                // beat the incumbent if it matches at the position that would
                // extend it. Guarded on `i + best_len` being a real position —
                // it is not when the incumbent already runs to end of input.
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
            // `as u16` are exact: best_len <= 258 and best_dist <= 32768.
            let (code, extra, extra_bits) = length_code(best_len as u16);
            let (lit_code, lit_len) = fixed_litlen(code);
            w.huffman(lit_code, lit_len);
            if extra_bits > 0 {
                w.bits(extra, extra_bits);
            }
            let (dcode, dextra, dextra_bits) = distance_code(best_dist as u16);
            // Distance codes are a FLAT 5-bit code in a fixed block, still
            // written most-significant bit first.
            w.huffman(dcode, 5);
            if dextra_bits > 0 {
                w.bits(dextra, dextra_bits);
            }
            // Insert every position the match covers, so later matches can find
            // them; this is what keeps the ratio from collapsing on repetitive
            // input.
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

    // End-of-block.
    let (code, len) = fixed_litlen(256);
    w.huffman(code, len);
    w.finish()
}

/// Wrap `data` as a complete zlib stream (RFC 1950): 2-byte header, the deflate
/// body, and the big-endian Adler-32 of the UNCOMPRESSED bytes.
pub(crate) fn zlib_compress(data: &[u8]) -> Vec<u8> {
    // CMF: CM = 8 (deflate), CINFO = 7 (32 KiB window). FLG: FLEVEL = 0, no
    // preset dictionary, and FCHECK chosen so the pair is a multiple of 31.
    // 0x78 0x01 satisfies that (0x7801 = 31 * 991).
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(&deflate_fixed(data));
    out.extend_from_slice(&super::checksum::adler32(data).to_be_bytes());
    out
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
        // Every byte value, so both fixed-code length ranges are exercised.
        let all: Vec<u8> = (0..=255u8).collect();
        roundtrip(&all);
        let repeated: Vec<u8> = all.iter().cycle().take(70_000).copied().collect();
        roundtrip(&repeated);
    }

    /// A maximal match (258 bytes) and a maximal distance (32768) must both be
    /// expressible — the two ends of the code tables.
    #[test]
    fn roundtrips_extreme_match_geometry() {
        let mut data = vec![7u8; 258];
        data.extend(std::iter::repeat_n(0u8, WINDOW - 258));
        data.extend(std::iter::repeat_n(7u8, 258));
        roundtrip(&data);
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

    /// Structured, image-like input. Two shapes, because they stress different
    /// halves of the matcher:
    ///
    /// * a raw RGBA gradient, which is only WEAKLY compressible byte-wise (its
    ///   per-pixel bytes all change) and is here for round-trip correctness;
    /// * the same image AFTER PNG filtering, which is what the encoder actually
    ///   hands this compressor — long runs of small residuals — and which must
    ///   compress hard or the encoder is not earning its place.
    #[test]
    fn roundtrips_image_like_input() {
        let mut raw = Vec::new();
        for y in 0..200u32 {
            raw.push(0); // filter byte
            for x in 0..200u32 {
                // `as u8`: the modulo keeps both in range.
                raw.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128, 255]);
            }
        }
        roundtrip(&raw);

        // A filtered flat/gradient region: filter byte + mostly-zero residuals,
        // the real shape of `encode::filter_image`'s output.
        let mut filtered = Vec::new();
        for _ in 0..200 {
            filtered.push(1u8); // Sub filter
            filtered.extend(std::iter::repeat_n(0u8, 400));
            filtered.extend((0..400u32).map(|i| ((i * 7) % 3) as u8));
        }
        roundtrip(&filtered);
        assert!(
            zlib_compress(&filtered).len() * 20 < filtered.len(),
            "filtered scanlines must compress at least 20x, got {} from {}",
            zlib_compress(&filtered).len(),
            filtered.len(),
        );
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
}
