// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The bit writer every entropy-coded field of a frame goes through.
//!
//! Fields are packed low bit first into little-endian bytes. An FSE table
//! description is read forward that way (RFC 8878 §4.1.1). Huffman and FSE
//! bitstreams are read backward, from the last byte down (RFC 8878 §4.1, §4.2.2): the
//! writer closes them with one 1 bit, the end marker a decoder finds first, so the
//! field written last is the one read first.

/// Packs bit fields into the tail of a byte vector.
pub(crate) struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    /// Bits not yet written, the oldest in the low end.
    acc: u64,
    /// How many bits of `acc` are live (at most 64).
    live: u32,
}

impl<'a> BitWriter<'a> {
    pub(crate) const fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            acc: 0,
            live: 0,
        }
    }

    /// Appends the low `bits` bits of `value` (`bits <= 56`).
    #[inline]
    pub(crate) fn add(&mut self, value: u64, bits: u32) {
        debug_assert!(bits <= 56, "{bits} bits in one field");
        if bits == 0 {
            return;
        }
        if self.live + bits > 64 {
            self.drain();
        }
        let mask = u64::MAX >> (64 - bits);
        self.acc |= (value & mask) << self.live;
        self.live += bits;
    }

    /// Writes out the whole bytes of `acc`.
    #[inline]
    fn drain(&mut self) {
        let bytes = self.live / 8;
        let word = self.acc.to_le_bytes();
        self.out
            .extend_from_slice(word.get(..bytes as usize).unwrap_or_default());
        self.acc = if bytes == 8 {
            0
        } else {
            self.acc >> (bytes * 8)
        };
        self.live -= bytes * 8;
    }

    /// Ends a stream read forward: the last partial byte, zero-padded.
    pub(crate) fn finish(mut self) {
        self.drain();
        if self.live > 0 {
            self.out.push(self.acc as u8);
        }
    }

    /// Ends a stream read backward: the end-marker bit, then [`Self::finish`].
    /// The last byte is never zero, so a decoder finds the marker in it (RFC 8878 §4.1).
    pub(crate) fn close(mut self) {
        self.add(1, 1);
        self.finish();
    }
}

#[cfg(test)]
#[path = "bits_tests.rs"]
mod tests;
