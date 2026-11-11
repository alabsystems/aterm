// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/bit_io/bit_reader.rs and
// src/bit_io/bit_reader_reverse.rs, modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

//! The two bit readers a frame needs (RFC 8878 §4.1).

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

    /// Bits read so far.
    pub(crate) const fn bits_read(&self) -> usize {
        self.idx
    }

    /// The next `n` bits (`n <= 32`), the first read in the lowest bit.
    pub(crate) fn get_bits(&mut self, n: usize) -> Result<u32, DecodeError> {
        if n > 32 {
            return Err(DecodeError::Corrupt("FSE table description ends early"));
        }
        let Some(end) = self.idx.checked_add(n) else {
            return Err(DecodeError::Corrupt("FSE table description ends early"));
        };
        if self.source.len().checked_mul(8).is_none_or(|cap| end > cap) {
            return Err(DecodeError::Corrupt("FSE table description ends early"));
        }
        let mut value: u64 = 0;
        let mut shift = 0;
        let mut at = self.idx;
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
        Ok(value as u32)
    }
}

/// Reads bit fields backward: from the highest bit of the last byte toward
/// the first byte.
pub(crate) struct BitReaderReversed<'s> {
    /// The index of the last byte loaded into `bit_container`.
    index: usize,
    /// How many bits of `bit_container` have been consumed.
    bits_consumed: u8,
    /// Bits consumed past the start of the input (read as zeros).
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
        let index_bits = isize::try_from(self.index)
            .unwrap_or(isize::MAX)
            .saturating_mul(8);
        let extra = isize::try_from(self.extra_bits).unwrap_or(isize::MAX);
        index_bits
            .saturating_add(64 - isize::from(self.bits_consumed))
            .saturating_sub(extra)
    }

    /// Skips the padding at the end of a stream: zero bits, then the one
    /// marker bit.
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
            let mut value = [0u8; 8];
            let head = self.source.len().min(8);
            if let (Some(dst), Some(src)) = (value.get_mut(..head), self.source.get(..head)) {
                dst.copy_from_slice(src);
            }
            self.bit_container = u64::from_le_bytes(value);
            self.bits_consumed -= 8 * self.index as u8;
            self.index = 0;
            self.bit_container <<= self.bits_consumed;
            self.extra_bits = self
                .extra_bits
                .saturating_add(usize::from(self.bits_consumed));
            self.bits_consumed = 0;
        } else if self.bits_consumed < 64 {
            self.bit_container <<= self.bits_consumed;
            self.extra_bits = self
                .extra_bits
                .saturating_add(usize::from(self.bits_consumed));
            self.bits_consumed = 0;
        } else {
            self.extra_bits = self
                .extra_bits
                .saturating_add(usize::from(self.bits_consumed));
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
#[path = "bits_tests.rs"]
mod tests;
