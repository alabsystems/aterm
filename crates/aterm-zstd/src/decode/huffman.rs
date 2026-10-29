// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/huff0/huff0_decoder.rs, modified by the aterm
// project in 2026. See ../../LICENSE-MIT for the upstream MIT license.

//! Huffman decoding of compressed literals (RFC 8878 §4.2).

use super::bits::BitReaderReversed;
use super::error::DecodeError;
use super::fse::{FseDecoder, FseTable, highest_bit_set};

const MAX_MAX_NUM_BITS: u8 = 11;

/// One state of a decoding table.
#[derive(Clone, Copy, Debug)]
struct Entry {
    symbol: u8,
    num_bits: u8,
}

/// A literals Huffman table, kept across blocks of a frame for Treeless literals.
pub(crate) struct HuffmanTable {
    decode: Vec<Entry>,
    weights: Vec<u8>,
    max_num_bits: u8,
    bits: Vec<u8>,
    bit_ranks: Vec<u32>,
    rank_indexes: Vec<usize>,
    fse_table: FseTable,
}

impl HuffmanTable {
    pub(crate) fn new() -> Self {
        Self {
            decode: Vec::new(),
            weights: Vec::with_capacity(256),
            max_num_bits: 0,
            bits: Vec::with_capacity(256),
            bit_ranks: Vec::with_capacity(12),
            rank_indexes: Vec::with_capacity(12),
            fse_table: FseTable::new(255),
        }
    }

    /// Forgets the table, keeping its allocations.
    pub(crate) fn reset(&mut self) {
        self.decode.clear();
        self.weights.clear();
        self.max_num_bits = 0;
        self.fse_table.reset();
    }

    /// Reads a tree description from the front of `source` and builds the table, returning the bytes read.
    pub(crate) fn build_decoder(&mut self, source: &[u8]) -> Result<usize, DecodeError> {
        self.decode.clear();
        self.max_num_bits = 0;
        let bytes_used = self.read_weights(source)?;
        self.build_table_from_weights()?;
        Ok(bytes_used)
    }

    fn read_weights(&mut self, source: &[u8]) -> Result<usize, DecodeError> {
        let Some((&header, rest)) = source.split_first() else {
            return Err(DecodeError::Corrupt("Huffman tree description is empty"));
        };
        self.weights.clear();

        if header < 128 {
            let Some(fse_stream) = rest.get(..usize::from(header)) else {
                return Err(DecodeError::Corrupt("Huffman weights run past the block"));
            };
            let table_bytes = self.fse_table.build_decoder(fse_stream, 6)?;
            let Some(compressed) = fse_stream.get(table_bytes..) else {
                return Err(DecodeError::Corrupt(
                    "Huffman weight table runs past its bytes",
                ));
            };
            let mut br = BitReaderReversed::new(compressed);
            br.skip_padding()?;

            let mut dec1 = FseDecoder::new(&self.fse_table);
            let mut dec2 = FseDecoder::new(&self.fse_table);
            dec1.init_state(&mut br)?;
            dec2.init_state(&mut br)?;
            if br.bits_remaining() < 0 {
                return Err(DecodeError::Corrupt(
                    "Huffman weight stream shorter than its initial states",
                ));
            }

            loop {
                self.weights.push(dec1.symbol());
                dec1.update_state(&mut br);
                if br.bits_remaining() <= -1 {
                    self.weights.push(dec2.symbol());
                    break;
                }
                self.weights.push(dec2.symbol());
                dec2.update_state(&mut br);
                if br.bits_remaining() <= -1 {
                    self.weights.push(dec1.symbol());
                    break;
                }
                if self.weights.len() > 255 {
                    return Err(DecodeError::Corrupt("too many Huffman weights"));
                }
            }
            Ok(1 + usize::from(header))
        } else {
            let num_weights = usize::from(header - 127);
            let bytes_needed = num_weights.div_ceil(2);
            let Some(raw) = rest.get(..bytes_needed) else {
                return Err(DecodeError::Corrupt("Huffman weights run past the block"));
            };
            for idx in 0..num_weights {
                let byte = raw.get(idx / 2).copied().unwrap_or(0);
                self.weights
                    .push(if idx % 2 == 0 { byte >> 4 } else { byte & 0xF });
            }
            Ok(1 + bytes_needed)
        }
    }

    fn build_table_from_weights(&mut self) -> Result<(), DecodeError> {
        if self.weights.len() > 255 {
            return Err(DecodeError::Corrupt("too many Huffman weights"));
        }
        let mut weight_sum: u32 = 0;
        for &w in &self.weights {
            if w > MAX_MAX_NUM_BITS {
                return Err(DecodeError::Corrupt("Huffman weight past the longest code"));
            }
            if w > 0 {
                weight_sum += 1 << (w - 1);
            }
        }
        if weight_sum == 0 {
            return Err(DecodeError::Corrupt("Huffman tree has no weights"));
        }
        let max_bits = highest_bit_set(weight_sum);
        let left_over = (1u32 << max_bits) - weight_sum;
        if !left_over.is_power_of_two() {
            return Err(DecodeError::Corrupt(
                "Huffman weights do not complete a tree",
            ));
        }
        if max_bits > u32::from(MAX_MAX_NUM_BITS) {
            return Err(DecodeError::Corrupt("Huffman code longer than 11 bits"));
        }
        let max_bits = max_bits as u8;
        let last_weight = highest_bit_set(left_over) as u8;

        self.bits.clear();
        for &w in &self.weights {
            self.bits.push(if w > 0 { max_bits + 1 - w } else { 0 });
        }
        self.bits.push(max_bits + 1 - last_weight);

        self.bit_ranks.clear();
        self.bit_ranks.resize(usize::from(max_bits) + 1, 0);
        for &nb in &self.bits {
            if let Some(rank) = self.bit_ranks.get_mut(usize::from(nb)) {
                *rank += 1;
            }
        }
        let longest = self
            .bit_ranks
            .get(usize::from(max_bits))
            .copied()
            .unwrap_or(0);
        if longest < 2 || longest % 2 != 0 {
            return Err(DecodeError::Corrupt(
                "Huffman tree has an odd count of longest codes",
            ));
        }

        self.rank_indexes.clear();
        self.rank_indexes.resize(usize::from(max_bits) + 1, 0);
        for nb in (1..=usize::from(max_bits)).rev() {
            let next = self.rank_indexes.get(nb).copied().unwrap_or(0)
                + self.bit_ranks.get(nb).copied().unwrap_or(0) as usize
                    * (1 << (usize::from(max_bits) - nb));
            if let Some(slot) = self.rank_indexes.get_mut(nb - 1) {
                *slot = next;
            }
        }

        let table_size = 1usize << max_bits;
        self.decode.clear();
        self.decode.resize(
            table_size,
            Entry {
                symbol: 0,
                num_bits: 0,
            },
        );
        for (symbol, &nb) in self.bits.iter().enumerate() {
            if nb == 0 {
                continue;
            }
            let Some(base) = self.rank_indexes.get_mut(usize::from(nb)) else {
                continue;
            };
            let len = 1usize << (max_bits - nb);
            let Some(run) = self.decode.get_mut(*base..*base + len) else {
                return Err(DecodeError::Corrupt("Huffman codes overfill the table"));
            };
            for entry in run {
                *entry = Entry {
                    symbol: symbol as u8,
                    num_bits: nb,
                };
            }
            *base += len;
        }
        self.max_num_bits = max_bits;
        Ok(())
    }

    /// Decodes literals from Huffman `streams` into `dsts` (RFC 8878 §4.2.2).
    pub(crate) fn decode_streams<const N: usize>(
        &self,
        streams: [&[u8]; N],
        mut dsts: [&mut [u8]; N],
    ) -> Result<(), DecodeError> {
        let max_bits = self.max_num_bits;
        if max_bits == 0 {
            return Err(DecodeError::Corrupt(
                "Treeless literals with no Huffman table",
            ));
        }
        let mut lanes = [(); N].map(|()| Lane::default());
        for (lane, stream) in lanes.iter_mut().zip(streams) {
            *lane = Lane::start(stream, max_bits)?;
        }
        let common = dsts.iter().map(|d| d.len()).min().unwrap_or(0);
        for j in 0..common {
            for (lane, dst) in lanes.iter_mut().zip(dsts.iter_mut()) {
                if let Some(byte) = dst.get_mut(j) {
                    *byte = lane.next(self);
                }
            }
        }
        for (lane, dst) in lanes.iter_mut().zip(dsts.iter_mut()) {
            for byte in dst.iter_mut().skip(common) {
                *byte = lane.next(self);
            }
            if lane.bits.bits_remaining() != -isize::from(max_bits) {
                return Err(DecodeError::Corrupt(
                    "Huffman stream length does not match its literals",
                ));
            }
        }
        Ok(())
    }
}

/// One Huffman stream being decoded: its bits, and a state of `max_num_bits`
/// bits that indexes the table.
#[derive(Default)]
struct Lane<'s> {
    bits: BitReaderReversed<'s>,
    state: u64,
    mask: u64,
}

impl<'s> Lane<'s> {
    fn start(stream: &'s [u8], max_bits: u8) -> Result<Self, DecodeError> {
        let mut bits = BitReaderReversed::new(stream);
        bits.skip_padding()?;
        let state = bits.get_bits(max_bits);
        Ok(Self {
            bits,
            state,
            mask: (1u64 << max_bits) - 1,
        })
    }

    /// The current state's symbol; then the next state, shifting in the
    /// bits the symbol's code took.
    #[inline(always)]
    fn next(&mut self, table: &HuffmanTable) -> u8 {
        let entry = usize::try_from(self.state)
            .ok()
            .and_then(|i| table.decode.get(i))
            .copied()
            .unwrap_or(Entry {
                symbol: 0,
                num_bits: 0,
            });
        let more = self.bits.get_bits(entry.num_bits);
        self.state = ((self.state << entry.num_bits) & self.mask) | more;
        entry.symbol
    }
}

#[cfg(test)]
mod tests {
    use super::HuffmanTable;

    #[test]
    fn direct_weights_build_the_canonical_table() {
        let mut t = HuffmanTable::new();
        let used = t.build_decoder(&[129, 0x11]).unwrap();
        assert_eq!(used, 2);
        assert_eq!(t.max_num_bits, 2);
        let syms: Vec<(u8, u8)> = t.decode.iter().map(|e| (e.symbol, e.num_bits)).collect();
        assert_eq!(syms, [(0, 2), (1, 2), (2, 1), (2, 1)]);
    }

    #[test]
    fn weights_that_complete_no_tree_are_refused() {
        let mut t = HuffmanTable::new();
        assert!(t.build_decoder(&[129, 0x22]).is_err());
        assert!(t.build_decoder(&[128]).is_err());
        assert!(t.build_decoder(&[]).is_err());
        assert!(t.build_decoder(&[129, 0x00]).is_err());
        assert!(t.build_decoder(&[128, 0xC0]).is_err());
        // FSE weight table (probs = [-1, 31], accuracy_log = 5) followed by a 0-bit stream (0x01)
        // lacks the 10 bits needed to initialize both FSE states and must be refused.
        assert!(t.build_decoder(&[3, 0x00, 0x7E, 0x01]).is_err());
    }
}
