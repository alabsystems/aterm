// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Huffman tables (ITU T.81 Annex C) and the entropy-coded segment bit reader (F.2.2.5).

/// Bits looked up in one step; longer codes take the canonical slow path.
const FAST_BITS: u32 = 9;

/// One DHT table, ready to decode.
#[derive(Clone)]
pub(crate) struct HuffmanTable {
    /// `(length << 8) | symbol` for codes up to [`FAST_BITS`] bits, or `0` when longer.
    fast: Vec<u16>,
    /// Left-aligned upper bound (`max_code + 1`) for each code length `1..=16`.
    maxcode: [u32; 18],
    /// For each length, the symbol index of its first code minus that code.
    delta: [i32; 17],
    symbols: Vec<u8>,
}

impl HuffmanTable {
    /// Builds a table from the sixteen per-length counts and symbols in code order.
    pub(crate) fn new(counts: &[u8; 16], symbols: &[u8]) -> Option<Self> {
        let total: usize = counts.iter().map(|&count| usize::from(count)).sum();
        if total == 0 || total > 256 || symbols.len() != total {
            return None;
        }
        let mut fast = vec![0u16; 1 << FAST_BITS];
        let mut maxcode = [0u32; 18];
        let mut delta = [0i32; 17];
        let mut code: u32 = 0;
        let mut index: usize = 0;
        for length in 1..=16u32 {
            let count = u32::from(counts[length as usize - 1]);
            delta[length as usize] = index as i32 - code as i32;
            if count > 0 {
                // A code of this length past 2^length is over-subscribed.
                if code + count > (1 << length) {
                    return None;
                }
                for _ in 0..count {
                    if length <= FAST_BITS {
                        let shift = FAST_BITS - length;
                        let first = (code << shift) as usize;
                        let entry = ((length as u16) << 8) | u16::from(symbols[index]);
                        for slot in fast.iter_mut().skip(first).take(1 << shift) {
                            *slot = entry;
                        }
                    }
                    code += 1;
                    index += 1;
                }
            }
            maxcode[length as usize] = code << (16 - length);
            code <<= 1;
        }
        maxcode[17] = u32::MAX;
        Some(Self {
            fast,
            maxcode,
            delta,
            symbols: symbols.to_vec(),
        })
    }
}

/// Bit reader for an entropy-coded JPEG segment, undoing `0xFF 0x00` byte stuffing.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// The next byte to load.
    pos: usize,
    /// Loaded bits, left-aligned (the next bit is bit 63).
    acc: u64,
    bits: u32,
    /// A marker (or the end of the data) was reached; `pos` is at its `0xFF`.
    ended: bool,
}

impl<'a> BitReader<'a> {
    #[inline]
    pub(crate) const fn new(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos,
            acc: 0,
            bits: 0,
            ended: false,
        }
    }

    /// Byte offset where the segment stops (at the terminating marker or end of input).
    #[inline]
    pub(crate) const fn position(&self) -> usize {
        self.pos
    }

    #[inline]
    fn refill(&mut self) {
        while self.bits <= 56 {
            let byte = if self.ended {
                0
            } else {
                match self.data.get(self.pos) {
                    None => {
                        self.ended = true;
                        0
                    }
                    Some(&0xFF) => match self.data.get(self.pos + 1) {
                        Some(&0x00) => {
                            self.pos += 2;
                            0xFF
                        }
                        Some(&0xFF) => {
                            self.pos += 1;
                            continue;
                        }
                        _ => {
                            self.ended = true;
                            0
                        }
                    },
                    Some(&byte) => {
                        self.pos += 1;
                        byte
                    }
                }
            };
            self.acc |= u64::from(byte) << (56 - self.bits);
            self.bits += 8;
        }
    }

    /// The next `n` (1..=16) bits as an unsigned number.
    #[inline]
    pub(crate) fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.bits < n {
            self.refill();
        }
        let value = (self.acc >> (64 - n)) as u32;
        self.acc <<= n;
        self.bits -= n;
        value
    }

    #[inline]
    pub(crate) fn bit(&mut self) -> bool {
        self.bits(1) != 0
    }

    /// `n` bits as a signed coefficient (T.81 F.2.2.1, EXTEND).
    #[inline]
    pub(crate) fn receive_extend(&mut self, n: u32) -> i32 {
        if n == 0 || n > 16 {
            return 0;
        }
        let value = self.bits(n) as i32;
        if value < (1 << (n - 1)) {
            value - (1 << n) + 1
        } else {
            value
        }
    }

    /// The next Huffman symbol, or `None` for a bit pattern no code has.
    #[inline]
    pub(crate) fn decode(&mut self, table: &HuffmanTable) -> Option<u8> {
        if self.bits < 16 {
            self.refill();
        }
        let window = (self.acc >> 48) as u32;
        let fast = table.fast[(window >> (16 - FAST_BITS)) as usize];
        if fast != 0 {
            let length = u32::from(fast >> 8);
            self.acc <<= length;
            self.bits -= length;
            return Some(fast as u8);
        }
        let mut length = FAST_BITS as usize + 1;
        while length <= 16 && window >= table.maxcode[length] {
            length += 1;
        }
        if length > 16 {
            return None;
        }
        let code = (window >> (16 - length)) as i32;
        let symbol = table
            .symbols
            .get(usize::try_from(code + table.delta[length]).ok()?)
            .copied()?;
        self.acc <<= length;
        self.bits -= length as u32;
        Some(symbol)
    }

    /// Drops buffered bits and advances past the next restart marker (`0xFFD0..=0xFFD7`).
    pub(crate) fn restart(&mut self) -> bool {
        self.acc = 0;
        self.bits = 0;
        let mut at = self.pos;
        loop {
            match (self.data.get(at), self.data.get(at + 1)) {
                (Some(&0xFF), Some(&marker)) if (0xD0..=0xD7).contains(&marker) => {
                    self.pos = at + 2;
                    self.ended = false;
                    return true;
                }
                (Some(&0xFF), Some(&marker)) if marker != 0x00 && marker != 0xFF => {
                    self.pos = at;
                    self.ended = true;
                    return false;
                }
                (Some(_), _) => at += 1,
                (None, _) => {
                    self.pos = at;
                    self.ended = true;
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BitReader, HuffmanTable};

    #[test]
    fn the_standard_dc_table_decodes_every_code() {
        let counts = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
        let symbols = [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let table = HuffmanTable::new(&counts, &symbols).expect("table");
        let mut reader = BitReader::new(&[0b0011_1111, 0b1100_0000], 0);
        assert_eq!(reader.decode(&table), Some(0));
        assert_eq!(reader.decode(&table), Some(11));
    }

    #[test]
    fn stuffing_is_undone_and_a_marker_ends_the_segment() {
        let mut reader = BitReader::new(&[0xFF, 0x00, 0xA5, 0xFF, 0xD9], 0);
        assert_eq!(reader.bits(8), 0xFF);
        assert_eq!(reader.bits(8), 0xA5);
        assert_eq!(reader.bits(16), 0);
        assert_eq!(reader.position(), 3, "stopped at the marker");
    }

    #[test]
    fn an_oversubscribed_code_is_refused() {
        let mut counts = [0u8; 16];
        counts[0] = 3;
        assert!(HuffmanTable::new(&counts, &[0, 1, 2]).is_none());
    }
}
