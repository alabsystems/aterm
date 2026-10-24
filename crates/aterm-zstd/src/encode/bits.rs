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
mod tests {
    use super::BitWriter;
    use crate::decode::bits::{BitReader, BitReaderReversed};

    /// Fields of every width from 0 to 56 bits, written then read back both ways the
    /// decoder reads: forward in order, and backward (after the end marker) in reverse.
    #[test]
    fn fields_read_back_forward_and_backward() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let fields: Vec<(u64, u32)> = (0..400)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let bits = (i * 7) % 57;
                (x & (u64::MAX >> (64 - bits.max(1))), bits)
            })
            .map(|(v, bits)| (if bits == 0 { 0 } else { v }, bits))
            .collect();

        let mut fwd = Vec::new();
        let mut w = BitWriter::new(&mut fwd);
        for &(v, bits) in &fields {
            w.add(v, bits);
        }
        w.finish();
        let total: u32 = fields.iter().map(|f| f.1).sum();
        assert_eq!(fwd.len(), total.div_ceil(8) as usize);
        let mut r = BitReader::new(&fwd);
        for &(v, bits) in &fields {
            let mut got = 0u64;
            let mut shift = 0;
            let mut left = bits;
            while left > 0 {
                let take = left.min(32);
                got |= u64::from(r.get_bits(take as usize).unwrap()) << shift;
                shift += take;
                left -= take;
            }
            assert_eq!(got, v, "{bits} bits forward");
        }

        let mut back = Vec::new();
        let mut w = BitWriter::new(&mut back);
        for &(v, bits) in &fields {
            w.add(v, bits);
        }
        w.close();
        assert_ne!(back.last(), Some(&0), "the end marker is in the last byte");
        let mut r = BitReaderReversed::new(&back);
        r.skip_padding().unwrap();
        for &(v, bits) in fields.iter().rev() {
            let mut got = 0u64;
            let mut left = bits;
            while left > 0 {
                let take = left.min(24) as u8;
                got = got << take | r.get_bits(take);
                left -= u32::from(take);
            }
            assert_eq!(got, v, "{bits} bits backward");
        }
        assert_eq!(r.bits_remaining(), 0);
    }

    /// The negative control: a field read back at the wrong width is not the field.
    #[test]
    fn a_field_read_at_the_wrong_width_differs() {
        let mut out = Vec::new();
        let mut w = BitWriter::new(&mut out);
        w.add(0b1011, 4);
        w.add(0b01, 2);
        w.close();
        let mut r = BitReaderReversed::new(&out);
        r.skip_padding().unwrap();
        assert_ne!(r.get_bits(3), 0b01);
    }
}
