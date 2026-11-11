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
/// The largest alphabet any Brotli prefix code uses (command alphabet = 704).
const MAX_ALPHABET: usize = 704;
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
        let mut codes_buf = [(0u32, 0u32); MAX_ALPHABET];
        let codes = &mut codes_buf[..lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l > 0 {
                let l = u32::from(l);
                codes[s] = (next[l as usize].reverse_bits() >> (32 - l), l);
                next[l as usize] += 1;
            }
        }
        let root = 1usize << ROOT;
        let mut sub_bits = [0u32; 1 << ROOT];
        for &(r, l) in &*codes {
            if l > ROOT {
                let p = (r as usize) & (root - 1);
                sub_bits[p] = sub_bits[p].max(l - ROOT);
            }
        }
        let total_len = root
            + sub_bits
                .iter()
                .map(|&b| if b > 0 { 1usize << b } else { 0 })
                .sum::<usize>();
        let mut table = vec![0u32; total_len];
        let mut next_at = root;
        for (p, &b) in sub_bits.iter().enumerate() {
            if b > 0 {
                table[p] = LINK | (b << 24) | next_at as u32;
                next_at += 1 << b;
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
    let mut lengths_buf = [0u8; MAX_ALPHABET];
    let lengths = &mut lengths_buf[..alphabet as usize];
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
    Code::canonical(lengths)
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
    let mut lengths_buf = [0u8; MAX_ALPHABET];
    let lengths = &mut lengths_buf[..alphabet as usize];
    for (&s, &l) in symbols[..n].iter().zip(shape) {
        lengths[s as usize] = l;
    }
    Code::canonical(lengths)
}

#[cfg(test)]
#[path = "prefix_tests.rs"]
mod tests;
