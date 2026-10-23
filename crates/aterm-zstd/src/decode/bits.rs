// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/bit_io/bit_reader.rs and
// src/bit_io/bit_reader_reverse.rs, modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

//! The two bit readers a frame needs (RFC 8878 §4.1).
//!
//! [`BitReader`] reads forward from the low bit of the first byte: FSE table
//! descriptions. [`BitReaderReversed`] reads backward from the high bit of the
//! last byte: the Huffman and FSE bitstreams, which an encoder writes forward
//! and a decoder must consume in the opposite order.
//!
//! Neither reader panics. A reversed read past the start returns zero bits
//! and counts them, so [`BitReaderReversed::bits_remaining`] goes negative and
//! the callers' end-of-stream checks refuse the stream.

use super::error::DecodeError;

/// Reads bit fields forward, low bit first.
pub(crate) struct BitReader<'s> {
    /// Bits already read.
    idx: usize,
    source: &'s [u8],
}

impl<'s> BitReader<'s> {
    pub(crate) const fn new(source: &'s [u8]) -> Self {
        Self { idx: 0, source }
    }

    /// Bits read so far, including any returned with [`Self::return_bit`].
    pub(crate) const fn bits_read(&self) -> usize {
        self.idx
    }

    /// Un-reads the last bit read. A no-op before the first read.
    pub(crate) const fn return_bit(&mut self) {
        self.idx = self.idx.saturating_sub(1);
    }

    /// The next `n` bits (`n <= 32`), the first read in the lowest bit.
    pub(crate) fn get_bits(&mut self, n: usize) -> Result<u32, DecodeError> {
        let end = self.idx + n;
        if n > 32 || end > self.source.len() * 8 {
            return Err(DecodeError::Corrupt("FSE table description ends early"));
        }
        let mut value: u64 = 0;
        let mut shift = 0;
        let mut at = self.idx;
        // At most 5 bytes hold any 32-bit field at any bit alignment.
        while at < end {
            let byte = self.source.get(at / 8).copied().unwrap_or(0);
            let skip = at % 8;
            let take = (8 - skip).min(end - at);
            let bits = (u64::from(byte) >> skip) & ((1 << take) - 1);
            value |= bits << shift;
            shift += take;
            at += take;
        }
        self.idx = end;
        // `n <= 32`, so the value fits.
        Ok(value as u32)
    }
}

/// Reads bit fields backward: from the highest bit of the last byte toward
/// the first byte. Zstandard's entropy streams are written so.
pub(crate) struct BitReaderReversed<'s> {
    /// The index of the last byte loaded into `bit_container`.
    index: usize,
    /// How many bits of `bit_container` have been consumed.
    bits_consumed: u8,
    /// Bits consumed past the start of the input (read as zeros). Zero until
    /// the input is exhausted.
    extra_bits: usize,
    source: &'s [u8],
    /// The bits not yet consumed sit in the high end, refilled byte-wise.
    bit_container: u64,
}

impl Default for BitReaderReversed<'_> {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl<'s> BitReaderReversed<'s> {
    pub(crate) const fn new(source: &'s [u8]) -> Self {
        Self {
            index: source.len(),
            bits_consumed: 64,
            source,
            bit_container: 0,
            extra_bits: 0,
        }
    }

    /// Bits left to read; negative once reads have run past the start.
    pub(crate) fn bits_remaining(&self) -> isize {
        self.index as isize * 8 + (64 - isize::from(self.bits_consumed)) - self.extra_bits as isize
    }

    /// Skips the padding at the end of a stream: zero bits, then the one
    /// marker bit. The last byte must hold the marker (at most 7 zeros first).
    pub(crate) fn skip_padding(&mut self) -> Result<(), DecodeError> {
        for _ in 0..8 {
            if self.get_bits(1) == 1 {
                return Ok(());
            }
        }
        Err(DecodeError::Corrupt(
            "bitstream has no end marker in its last byte",
        ))
    }

    /// Moves the unread bits to the top of the container and loads whole
    /// bytes below them, so at least 56 bits can be read afterwards.
    #[inline(always)]
    fn refill(&mut self) {
        let bytes_consumed = usize::from(self.bits_consumed / 8);
        if bytes_consumed == 0 {
            return;
        }
        // The common case, inline: eight more bytes below the window. (The
        // window starts at the last eight bytes and only moves down, so the
        // load is in range whenever the index has room to move.)
        if self.index >= bytes_consumed {
            let at = self.index - bytes_consumed;
            if let Some(chunk) = self.source.get(at..).and_then(<[u8]>::first_chunk::<8>) {
                self.index = at;
                self.bits_consumed &= 7;
                self.bit_container = u64::from_le_bytes(*chunk);
                return;
            }
        }
        self.refill_slow(bytes_consumed);
    }

    /// The input's first bytes, and past them: zeros.
    #[cold]
    fn refill_slow(&mut self, bytes_consumed: usize) {
        if self.index >= bytes_consumed {
            // The window moves down by whole bytes; a partly read byte is now
            // in the high bits of the container.
            self.index -= bytes_consumed;
            self.bits_consumed &= 7;
            if let Some(chunk) = self
                .source
                .get(self.index..)
                .and_then(|rest| rest.first_chunk::<8>())
            {
                self.bit_container = u64::from_le_bytes(*chunk);
            }
        } else if self.index > 0 {
            // Fewer than 8 unread bytes: load the start of the input, then
            // shift in zeros for the bytes that are not there.
            let mut value = [0u8; 8];
            let head = self.source.len().min(8);
            if let (Some(dst), Some(src)) = (value.get_mut(..head), self.source.get(..head)) {
                dst.copy_from_slice(src);
            }
            self.bit_container = u64::from_le_bytes(value);
            // `index < bytes_consumed <= 8`, so the product fits a u8.
            self.bits_consumed -= 8 * self.index as u8;
            self.index = 0;
            self.bit_container <<= self.bits_consumed;
            self.extra_bits += usize::from(self.bits_consumed);
            self.bits_consumed = 0;
        } else if self.bits_consumed < 64 {
            // The input is exhausted: shift out the used bits, fill zeros.
            self.bit_container <<= self.bits_consumed;
            self.extra_bits += usize::from(self.bits_consumed);
            self.bits_consumed = 0;
        } else {
            self.extra_bits += usize::from(self.bits_consumed);
            self.bits_consumed = 0;
            self.bit_container = 0;
        }
    }

    /// The next `n` bits (`n <= 56`); zeros past the start of the input.
    #[inline(always)]
    pub(crate) fn get_bits(&mut self, n: u8) -> u64 {
        if self.bits_consumed + n > 64 {
            self.refill();
        }
        let value = self.peek_bits(n);
        self.consume(n);
        value
    }

    #[inline(always)]
    const fn peek_bits(&self, n: u8) -> u64 {
        if n == 0 {
            return 0;
        }
        let mask = (1u64 << n) - 1;
        let shift = 64 - self.bits_consumed - n;
        (self.bit_container >> shift) & mask
    }

    /// The next `n1`, `n2` and `n3` bits without consuming them, where
    /// `sum = n1 + n2 + n3` bits are loaded.
    #[inline(always)]
    const fn peek_bits_triple(&self, sum: u8, n1: u8, n2: u8, n3: u8) -> (u64, u64, u64) {
        if sum == 0 {
            return (0, 0, 0);
        }
        // |consumed..|n1 bits|n2 bits|n3 bits|, low bits to the right.
        let all = self.bit_container >> (64 - self.bits_consumed - sum);
        let val1 = (all >> (n3 + n2)) & ((1u64 << n1) - 1);
        let val2 = (all >> n3) & ((1u64 << n2) - 1);
        let val3 = all & ((1u64 << n3) - 1);
        (val1, val2, val3)
    }

    #[inline(always)]
    const fn consume(&mut self, n: u8) {
        self.bits_consumed += n;
    }

    /// Three [`Self::get_bits`] in one refill when they fit 56 bits.
    #[inline(always)]
    pub(crate) fn get_bits_triple(&mut self, n1: u8, n2: u8, n3: u8) -> (u64, u64, u64) {
        let sum = n1 + n2 + n3;
        if sum <= 56 {
            self.refill();
            let triple = self.peek_bits_triple(sum, n1, n2, n3);
            self.consume(sum);
            return triple;
        }
        (self.get_bits(n1), self.get_bits(n2), self.get_bits(n3))
    }
}

#[cfg(test)]
mod tests {
    use super::{BitReader, BitReaderReversed};

    #[test]
    fn reversed_reads_from_the_last_byte_down_then_zeros() {
        let data = [0b1010_1010, 0b0101_0101];
        let mut br = BitReaderReversed::new(&data);
        assert_eq!(br.get_bits(1), 0);
        assert_eq!(br.get_bits(1), 1);
        assert_eq!(br.get_bits(1), 0);
        assert_eq!(br.get_bits(4), 0b1010);
        assert_eq!(br.get_bits(4), 0b1101);
        assert_eq!(br.get_bits(4), 0b0101);
        // The last 0 of the input, then three zeros filled in.
        assert_eq!(br.get_bits(4), 0b0000);
        assert_eq!(br.get_bits(4), 0b0000);
        assert_eq!(br.bits_remaining(), -7);
    }

    #[test]
    fn reversed_triple_matches_three_single_reads() {
        let data: Vec<u8> = (0u8..40).map(|i| i.wrapping_mul(37) ^ 0x5A).collect();
        for (n1, n2, n3) in [(0, 0, 0), (5, 9, 16), (31, 16, 16), (31, 0, 9), (1, 1, 1)] {
            let mut a = BitReaderReversed::new(&data);
            let mut b = BitReaderReversed::new(&data);
            for _ in 0..4 {
                let want = (a.get_bits(n1), a.get_bits(n2), a.get_bits(n3));
                assert_eq!(b.get_bits_triple(n1, n2, n3), want, "{n1} {n2} {n3}");
            }
            assert_eq!(a.bits_remaining(), b.bits_remaining());
        }
    }

    #[test]
    fn padding_needs_a_marker_in_the_last_byte() {
        let mut br = BitReaderReversed::new(&[0xFF, 0x01]);
        br.skip_padding().unwrap();
        assert_eq!(br.bits_remaining(), 8);
        let mut br = BitReaderReversed::new(&[0xFF, 0x00]);
        assert!(br.skip_padding().is_err());
        let mut br = BitReaderReversed::new(&[]);
        assert!(br.skip_padding().is_err());
    }

    #[test]
    fn forward_reads_low_bits_first_across_bytes() {
        let data = [0b1011_0110, 0b1100_0011, 0xFF];
        let mut br = BitReader::new(&data);
        assert_eq!(br.get_bits(3).unwrap(), 0b110);
        assert_eq!(br.get_bits(7).unwrap(), 0b111_0110);
        br.return_bit();
        assert_eq!(br.bits_read(), 9);
        assert_eq!(br.get_bits(15).unwrap(), 0b111_1111_1110_0001);
        assert!(br.get_bits(1).is_err());
        assert_eq!(BitReader::new(&[]).get_bits(0).unwrap(), 0);
    }
}
