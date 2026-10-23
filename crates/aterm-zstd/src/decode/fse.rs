// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/fse/fse_decoder.rs, modified by the aterm
// project in 2026. See ../../LICENSE-MIT for the upstream MIT license.

//! Finite State Entropy decoding (RFC 8878 §4.1).

use super::bits::{BitReader, BitReaderReversed};
use super::error::DecodeError;

const ACC_LOG_OFFSET: u8 = 5;

/// One state of a decoding table.
#[derive(Clone, Copy, Debug)]
struct Entry {
    /// Added to the bits read to give the next state.
    base_line: u32,
    /// Bits to read for the next state.
    num_bits: u8,
    /// The symbol this state decodes to.
    symbol: u8,
}

impl Entry {
    const ZERO: Self = Self {
        base_line: 0,
        num_bits: 0,
        symbol: 0,
    };
}

/// A decoding table for symbols `0..=max_symbol`.
#[derive(Clone, Debug)]
pub(crate) struct FseTable {
    max_symbol: u8,
    decode: Vec<Entry>,
    /// The table holds `1 << accuracy_log` states; 0 means not built.
    accuracy_log: u8,
    /// Per symbol: its share of the table, or -1 for "less than one".
    probs: Vec<i32>,
    /// Scratch while building: states handed out per symbol.
    counter: Vec<u32>,
}

impl FseTable {
    pub(crate) fn new(max_symbol: u8) -> Self {
        Self {
            max_symbol,
            decode: Vec::new(),
            accuracy_log: 0,
            probs: Vec::with_capacity(256),
            counter: Vec::with_capacity(256),
        }
    }

    /// Forgets the table, keeping its allocations.
    pub(crate) fn reset(&mut self) {
        self.decode.clear();
        self.probs.clear();
        self.accuracy_log = 0;
    }

    /// Whether a table has been built (a Repeat mode may reuse it).
    pub(crate) const fn is_built(&self) -> bool {
        self.accuracy_log != 0
    }

    /// `log2` of the table size; 0 when not built.
    pub(crate) const fn accuracy_log(&self) -> u8 {
        self.accuracy_log
    }

    /// Each state's `(symbol, num_bits, base_line)`, in state order.
    pub(crate) fn states(&self) -> impl Iterator<Item = (u8, u8, u32)> + '_ {
        self.decode
            .iter()
            .map(|e| (e.symbol, e.num_bits, e.base_line))
    }

    /// Reads a table description from the front of `source` and builds the table.
    pub(crate) fn build_decoder(
        &mut self,
        source: &[u8],
        max_log: u8,
    ) -> Result<usize, DecodeError> {
        self.accuracy_log = 0;
        let bytes_read = self.read_probabilities(source, max_log)?;
        self.build_decoding_table()?;
        Ok(bytes_read)
    }

    /// Builds the table from a known distribution (the predefined ones).
    pub(crate) fn build_from_probabilities(
        &mut self,
        acc_log: u8,
        probs: &[i32],
    ) -> Result<(), DecodeError> {
        self.probs.clear();
        self.probs.extend_from_slice(probs);
        self.accuracy_log = acc_log;
        self.build_decoding_table()
    }

    fn build_decoding_table(&mut self) -> Result<(), DecodeError> {
        let Self {
            max_symbol,
            decode,
            accuracy_log,
            probs,
            counter,
        } = self;
        let acc_log = *accuracy_log;
        if acc_log == 0 || acc_log > 9 {
            *accuracy_log = 0;
            return Err(DecodeError::Corrupt("FSE accuracy log out of range"));
        }
        if probs.len() > usize::from(*max_symbol) + 1 {
            *accuracy_log = 0;
            return Err(DecodeError::Corrupt(
                "FSE table has more symbols than allowed",
            ));
        }
        let table_size = 1usize << acc_log;
        let total: i64 = probs
            .iter()
            .map(|&p| i64::from(p.max(0)) + i64::from(p == -1))
            .sum();
        if total != table_size as i64 || probs.iter().any(|&p| p < -1) {
            *accuracy_log = 0;
            return Err(DecodeError::Corrupt(
                "FSE probabilities do not fill the table",
            ));
        }

        decode.clear();
        decode.resize(table_size, Entry::ZERO);

        let mut negative_idx = table_size;
        for (symbol, &p) in probs.iter().enumerate() {
            if p == -1 {
                negative_idx -= 1;
                if let Some(entry) = decode.get_mut(negative_idx) {
                    *entry = Entry {
                        base_line: 0,
                        num_bits: acc_log,
                        symbol: symbol as u8,
                    };
                }
            }
        }

        let mut position = 0;
        for (symbol, &p) in probs.iter().enumerate() {
            for _ in 0..p.max(0) {
                if let Some(entry) = decode.get_mut(position) {
                    entry.symbol = symbol as u8;
                }
                position = next_position(position, table_size);
                while position >= negative_idx {
                    position = next_position(position, table_size);
                }
            }
        }

        counter.clear();
        counter.resize(probs.len(), 0);
        for entry in decode.iter_mut().take(negative_idx) {
            let symbol = usize::from(entry.symbol);
            let (Some(&prob), Some(count)) = (probs.get(symbol), counter.get_mut(symbol)) else {
                return Err(DecodeError::Corrupt("FSE state names no symbol"));
            };
            let (base_line, num_bits) =
                calc_baseline_and_numbits(table_size as u32, prob.max(0) as u32, *count);
            if num_bits > acc_log {
                *accuracy_log = 0;
                return Err(DecodeError::Corrupt(
                    "FSE state reads more bits than the table has",
                ));
            }
            *count += 1;
            entry.base_line = base_line;
            entry.num_bits = num_bits;
        }
        Ok(())
    }

    fn read_probabilities(&mut self, source: &[u8], max_log: u8) -> Result<usize, DecodeError> {
        self.probs.clear();
        let mut br = BitReader::new(source);
        self.accuracy_log = ACC_LOG_OFFSET + br.get_bits(4)? as u8;
        if self.accuracy_log > max_log {
            self.accuracy_log = 0;
            return Err(DecodeError::Corrupt("FSE accuracy log past the maximum"));
        }
        let probability_sum: u32 = 1 << self.accuracy_log;
        let mut probability_counter: u32 = 0;

        while probability_counter < probability_sum {
            let max_remaining_value = probability_sum - probability_counter + 1;
            let bits_to_read = highest_bit_set(max_remaining_value);
            let unchecked_value = br.get_bits(bits_to_read as usize)?;

            let low_threshold = ((1u32 << bits_to_read) - 1) - max_remaining_value;
            let mask = (1u32 << (bits_to_read - 1)) - 1;
            let small_value = unchecked_value & mask;
            let value = if small_value < low_threshold {
                br.return_bit();
                small_value
            } else if unchecked_value > mask {
                unchecked_value - low_threshold
            } else {
                unchecked_value
            };

            let prob = value as i32 - 1;
            self.probs.push(prob);
            if prob > 0 {
                probability_counter += prob as u32;
            } else if prob == -1 {
                probability_counter += 1;
            } else {
                loop {
                    let skip = br.get_bits(2)? as usize;
                    self.probs.resize(self.probs.len() + skip, 0);
                    if self.probs.len() > usize::from(self.max_symbol) + 1 {
                        self.accuracy_log = 0;
                        return Err(DecodeError::Corrupt(
                            "FSE table has more symbols than allowed",
                        ));
                    }
                    if skip != 3 {
                        break;
                    }
                }
            }
            if self.probs.len() > usize::from(self.max_symbol) + 1 {
                self.accuracy_log = 0;
                return Err(DecodeError::Corrupt(
                    "FSE table has more symbols than allowed",
                ));
            }
        }

        if probability_counter != probability_sum {
            self.accuracy_log = 0;
            return Err(DecodeError::Corrupt(
                "FSE probabilities overshoot the table",
            ));
        }
        Ok(br.bits_read().div_ceil(8))
    }
}

/// Decodes symbols with one [`FseTable`].
pub(crate) struct FseDecoder<'t> {
    state: Entry,
    table: &'t FseTable,
}

impl<'t> FseDecoder<'t> {
    pub(crate) fn new(table: &'t FseTable) -> Self {
        Self {
            state: table.decode.first().copied().unwrap_or(Entry::ZERO),
            table,
        }
    }

    /// The symbol of the current state.
    #[inline(always)]
    pub(crate) const fn symbol(&self) -> u8 {
        self.state.symbol
    }

    /// Reads the first state: `accuracy_log` bits.
    pub(crate) fn init_state(
        &mut self,
        bits: &mut BitReaderReversed<'_>,
    ) -> Result<(), DecodeError> {
        if !self.table.is_built() {
            return Err(DecodeError::Corrupt(
                "FSE repeat mode with no table to repeat",
            ));
        }
        let state = bits.get_bits(self.table.accuracy_log);
        self.state = self.entry(state);
        Ok(())
    }

    /// Moves to the next state: base line plus `num_bits` read.
    #[inline(always)]
    pub(crate) fn update_state(&mut self, bits: &mut BitReaderReversed<'_>) {
        let add = bits.get_bits(self.state.num_bits);
        self.state = self.entry(u64::from(self.state.base_line) + add);
    }

    /// A built table holds every state its entries can reach, so the
    /// fallback is never taken; it keeps the lookup panic-free.
    #[inline(always)]
    fn entry(&self, state: u64) -> Entry {
        usize::try_from(state)
            .ok()
            .and_then(|i| self.table.decode.get(i))
            .copied()
            .unwrap_or(Entry::ZERO)
    }
}

/// `x`'s bit length; 0 for 0.
pub(crate) const fn highest_bit_set(x: u32) -> u32 {
    u32::BITS - x.leading_zeros()
}

const fn next_position(p: usize, table_size: usize) -> usize {
    (p + (table_size >> 1) + (table_size >> 3) + 3) & (table_size - 1)
}

const fn calc_baseline_and_numbits(
    num_states_total: u32,
    num_states_symbol: u32,
    state_number: u32,
) -> (u32, u8) {
    if num_states_symbol == 0 {
        return (0, 0);
    }
    let num_state_slices = num_states_symbol.next_power_of_two();
    let num_double_width = num_state_slices - num_states_symbol;
    let num_single_width = num_states_symbol - num_double_width;
    let slice_width = num_states_total / num_state_slices;
    let num_bits = highest_bit_set(slice_width).saturating_sub(1) as u8;
    if state_number < num_double_width {
        let base_line = num_single_width * slice_width + state_number * slice_width * 2;
        (base_line, num_bits + 1)
    } else {
        ((state_number - num_double_width) * slice_width, num_bits)
    }
}

#[cfg(test)]
mod tests {
    use super::FseTable;
    use crate::decode::sequences::{LL_DEFAULT_ACC_LOG, LL_DEFAULT_DISTRIBUTION};

    #[test]
    fn predefined_literal_length_table() {
        let mut table = FseTable::new(35);
        table
            .build_from_probabilities(LL_DEFAULT_ACC_LOG, &LL_DEFAULT_DISTRIBUTION)
            .unwrap();
        assert_eq!(table.decode.len(), 64);
        let row = |i: usize| {
            let e = table.decode[i];
            (e.symbol, e.num_bits, e.base_line)
        };
        assert_eq!(row(0), (0, 4, 0));
        assert_eq!(row(1), (0, 4, 16));
        assert_eq!(row(19), (27, 6, 0));
        assert_eq!(row(39), (25, 4, 16));
        assert_eq!(row(59), (24, 5, 32));
        assert_eq!(row(60), (35, 6, 0));
        assert_eq!(row(63), (32, 6, 0));
    }

    #[test]
    fn distributions_that_do_not_fill_the_table_are_refused() {
        let mut table = FseTable::new(35);
        assert!(table.build_from_probabilities(5, &[16, 15]).is_err());
        assert!(!table.is_built());
        assert!(table.build_from_probabilities(5, &[16, 17]).is_err());
        assert!(table.build_from_probabilities(5, &[-2, 16, 18]).is_err());
        assert!(table.build_from_probabilities(5, &[-1; 32]).is_ok());
        assert!(table.build_from_probabilities(5, &[1; 37]).is_err());
    }
}
