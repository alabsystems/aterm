// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Prefix codes (RFC 7932 §3): reading their two representations, and decoding symbols
//! through a two-level lookup table.

use std::io::Read;

use crate::bits::Bits;
use crate::error::{Step, corrupt};
use crate::tables::{CODE_LENGTH_CODE, CODE_LENGTH_ORDER};

/// Bits the first table level resolves.
const ROOT: u32 = 8;
/// The longest code (§3.5).
const MAX_LEN: u8 = 15;
/// A table entry linking to a second-level table.
const LINK: u32 = 1 << 31;

/// A prefix code as a lookup table. An entry holds a symbol in bits 0..16 and the bits it
/// takes in bits 16..24; a first-level entry with [`LINK`] set holds instead the offset of
/// its second-level table in bits 0..16 and that table's index width in bits 24..28.
pub(crate) struct Code {
    table: Vec<u32>,
}

impl Code {
    /// The code of one symbol, which takes no bits.
    fn single(symbol: u32) -> Self {
        Self {
            table: vec![symbol; 1 << ROOT],
        }
    }

    /// The canonical code (§3.2) of `lengths`, symbol by symbol; it must be complete
    /// unless exactly one length is non-zero.
    fn canonical(lengths: &[u8]) -> Step<Self> {
        let mut count = [0u32; MAX_LEN as usize + 1];
        let mut used = 0;
        let mut last = 0;
        for (s, &l) in lengths.iter().enumerate() {
            if l > MAX_LEN {
                return corrupt("a code length is over 15");
            }
            if l > 0 {
                count[usize::from(l)] += 1;
                used += 1;
                last = s;
            }
        }
        match used {
            0 => return corrupt("a prefix code has no symbol"),
            1 => return Ok(Self::single(last as u32)),
            _ => {}
        }
        let mut left: i64 = 1;
        for &c in &count[1..] {
            left = (left << 1) - i64::from(c);
            if left < 0 {
                return corrupt("a prefix code is over-subscribed");
            }
        }
        if left != 0 {
            return corrupt("a prefix code is incomplete");
        }
        let mut next = [0u32; MAX_LEN as usize + 1];
        let mut code = 0u32;
        for len in 1..=usize::from(MAX_LEN) {
            code = (code + count[len - 1]) << 1;
            next[len] = code;
        }
        // Each symbol's code, bit-reversed: the order the stream sends it in.
        let mut codes = vec![(0u32, 0u32); lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l > 0 {
                let l = u32::from(l);
                codes[s] = (next[l as usize].reverse_bits() >> (32 - l), l);
                next[l as usize] += 1;
            }
        }
        let root = 1usize << ROOT;
        let mut sub_bits = vec![0u32; root];
        for &(r, l) in &codes {
            if l > ROOT {
                let p = (r as usize) & (root - 1);
                sub_bits[p] = sub_bits[p].max(l - ROOT);
            }
        }
        let mut table = vec![0u32; root];
        for p in 0..root {
            if sub_bits[p] > 0 {
                let at = table.len() as u32;
                table[p] = LINK | (sub_bits[p] << 24) | at;
                table.resize(table.len() + (1 << sub_bits[p]), 0);
            }
        }
        for (s, &(r, l)) in codes.iter().enumerate() {
            if l == 0 {
                continue;
            }
            if l <= ROOT {
                let mut k = r as usize;
                while k < root {
                    table[k] = (l << 16) | s as u32;
                    k += 1 << l;
                }
            } else {
                let p = (r as usize) & (root - 1);
                let (at, width) = ((table[p] & 0xffff) as usize, sub_bits[p]);
                let mut k = (r >> ROOT) as usize;
                while k < 1 << width {
                    table[at + k] = ((l - ROOT) << 16) | s as u32;
                    k += 1 << (l - ROOT);
                }
            }
        }
        Ok(Self { table })
    }

    /// The next symbol.
    #[inline]
    pub(crate) fn decode<R: Read>(&self, b: &mut Bits<R>) -> Step<u32> {
        let p = b.peek15()?;
        let e = self.table[(p & 0xff) as usize];
        if e & LINK == 0 {
            b.consume((e >> 16) & 0xff)?;
            return Ok(e & 0xffff);
        }
        let width = (e >> 24) & 0xf;
        let at = (e & 0xffff) as usize;
        let e = self.table[at + ((p >> ROOT) & ((1 << width) - 1)) as usize];
        b.consume(ROOT + ((e >> 16) & 0xff))?;
        Ok(e & 0xffff)
    }
}

/// Bits a simple code's symbol takes: enough for `alphabet - 1` (§3.4).
const fn symbol_bits(alphabet: u32) -> u32 {
    32 - (alphabet - 1).leading_zeros()
}

/// Read one prefix code over `alphabet` symbols, in either representation (§3.4, §3.5).
pub(crate) fn read_code<R: Read>(b: &mut Bits<R>, alphabet: u32) -> Step<Code> {
    let hskip = b.read(2)?;
    if hskip == 1 {
        return read_simple(b, alphabet);
    }
    let mut lengths_code = [0u8; 18];
    let mut space: i32 = 32;
    let mut used = 0;
    for &at in &CODE_LENGTH_ORDER[hskip as usize..] {
        let p = b.peek15()? & 0xf;
        let (bits, len) = CODE_LENGTH_CODE[p as usize];
        b.consume(bits)?;
        lengths_code[at] = len;
        if len != 0 {
            space -= 32 >> len;
            used += 1;
            if space <= 0 {
                break;
            }
        }
    }
    if used != 1 && space != 0 {
        return corrupt("the code length code is not complete");
    }
    let lengths_code = Code::canonical(&lengths_code)?;
    let mut lengths = vec![0u8; alphabet as usize];
    let mut symbol = 0usize;
    let mut previous: u8 = 8;
    let mut repeat: u32 = 0;
    let mut repeat_len: u8 = 0;
    let mut space: i32 = 1 << 15;
    while symbol < lengths.len() && space > 0 {
        let c = lengths_code.decode(b)?;
        if c < 16 {
            repeat = 0;
            lengths[symbol] = c as u8;
            if c != 0 {
                previous = c as u8;
                space -= (1 << 15) >> c;
            }
            symbol += 1;
            continue;
        }
        let (extra, len) = if c == 16 { (2, previous) } else { (3, 0) };
        if repeat_len != len {
            repeat = 0;
            repeat_len = len;
        }
        let before = repeat;
        if repeat > 0 {
            repeat = (repeat - 2) << extra;
        }
        repeat += b.read(extra)? + 3;
        let delta = (repeat - before) as usize;
        if symbol + delta > lengths.len() {
            return corrupt("a code length repeat runs past the alphabet");
        }
        lengths[symbol..symbol + delta].fill(repeat_len);
        symbol += delta;
        if repeat_len != 0 {
            space -= (delta as i32) << (15 - u32::from(repeat_len));
        }
    }
    if space != 0 {
        return corrupt("a prefix code's lengths are not complete");
    }
    Code::canonical(&lengths)
}

fn read_simple<R: Read>(b: &mut Bits<R>, alphabet: u32) -> Step<Code> {
    let n = b.read(2)? as usize + 1;
    let width = symbol_bits(alphabet);
    let mut symbols = [0u32; 4];
    for i in 0..n {
        let s = b.read(width)?;
        if s >= alphabet {
            return corrupt("a simple code's symbol is outside its alphabet");
        }
        if symbols[..i].contains(&s) {
            return corrupt("a simple code names a symbol twice");
        }
        symbols[i] = s;
    }
    let shape: &[u8] = match n {
        1 => return Ok(Code::single(symbols[0])),
        2 => &[1, 1],
        3 => &[1, 2, 2],
        _ if b.read(1)? == 0 => &[2, 2, 2, 2],
        _ => &[1, 2, 3, 3],
    };
    let mut lengths = vec![0u8; alphabet as usize];
    for (&s, &l) in symbols[..n].iter().zip(shape) {
        lengths[s as usize] = l;
    }
    Code::canonical(&lengths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(bytes: &[u8]) -> Bits<&[u8]> {
        Bits::new(bytes)
    }

    /// A complete code decodes each symbol from the bits its canonical code assigns, a
    /// second-level table included (lengths past 8 bits), and refuses an incomplete or
    /// over-subscribed one.
    #[test]
    fn canonical_codes_decode_their_symbols_and_bad_ones_are_refused() {
        // Symbols 0..=9: lengths 1, 2, ..., 9, 9 — complete, the two longest past ROOT.
        let lengths = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 9];
        let code = Code::canonical(&lengths).unwrap();
        // Canonical codes: 0 -> "0", 1 -> "10", ..., 8 -> "111111110", 9 -> "111111111";
        // sent first bit first, least significant first in the stream.
        for (symbol, &len) in lengths.iter().enumerate() {
            let ones = if symbol == 9 { len } else { len - 1 };
            let mut stream = 0u32;
            for i in 0..u32::from(ones) {
                stream |= 1 << i;
            }
            let bytes = stream.to_le_bytes();
            let mut b = bits(&bytes);
            assert_eq!(code.decode(&mut b).unwrap(), symbol as u32, "{symbol}");
            assert_eq!(b.position(), u64::from(len), "{symbol} took its length");
        }
        assert!(Code::canonical(&[1, 2, 3]).is_err(), "incomplete");
        assert!(Code::canonical(&[1, 1, 1]).is_err(), "over-subscribed");
        assert!(Code::canonical(&[0, 0]).is_err(), "empty");
        assert!(Code::canonical(&[16, 1]).is_err(), "too long");
        let one = Code::canonical(&[0, 0, 3, 0]).unwrap();
        assert_eq!(
            one.decode(&mut bits(&[])).unwrap(),
            2,
            "one symbol takes no bits"
        );
    }
}
