// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Huffman coding of literals (RFC 8878 §4.2): code lengths from a histogram, the tree
//! description (§4.2.1), and the coded streams (§4.2.2).

use super::bits::BitWriter;
use super::fse;

/// The longest code a description can name (RFC 8878 §4.2.1: `Max_Number_of_Bits` is
/// at most 11).
pub(crate) const MAX_BITS: u32 = 11;

/// The largest accuracy log of an FSE-compressed weight table (RFC 8878 §4.2.1.2).
const WEIGHT_LOG: u32 = 6;

/// A literals Huffman code: what a `Compressed_Literals_Block` describes and a
/// `Treeless_Literals_Block` reuses.
#[derive(Clone, Debug)]
pub(crate) struct Code {
    /// Each byte's code length; 0 for a byte the code does not cover.
    lens: [u8; 256],
    codes: [u16; 256],
    max_bits: u8,
    /// The largest byte covered: the one whose weight a description leaves implied.
    last: u8,
}

impl Code {
    /// The code for `counts`, or `None` when fewer than two bytes occur (no prefix code
    /// exists; such literals are an RLE section).
    pub(crate) fn new(counts: &[u32; 256]) -> Option<Self> {
        let lens = lengths(counts, MAX_BITS)?;
        let max_bits = lens.iter().copied().max().unwrap_or(0);
        let last = lens.iter().rposition(|&l| l > 0)? as u8;
        // Canonical codes in a decoder's order (RFC 8878 §4.2.1.3): the longest codes
        // first, each length's bytes in byte order.
        let mut per_len = [0u32; MAX_BITS as usize + 2];
        for &l in &lens {
            if let Some(n) = per_len.get_mut(usize::from(l)) {
                *n += 1;
            }
        }
        let mut start = [0u32; MAX_BITS as usize + 2];
        let mut at = 0u32;
        for l in (1..=usize::from(max_bits)).rev() {
            if let Some(s) = start.get_mut(l) {
                *s = at;
            }
            at += per_len.get(l).copied().unwrap_or(0) << (u32::from(max_bits) - l as u32);
        }
        debug_assert_eq!(at, 1 << max_bits, "the code is complete");
        let mut codes = [0u16; 256];
        for (code, &l) in codes.iter_mut().zip(&lens) {
            if l == 0 {
                continue;
            }
            let shift = u32::from(max_bits - l);
            if let Some(s) = start.get_mut(usize::from(l)) {
                *code = (*s >> shift) as u16;
                *s += 1 << shift;
            }
        }
        Some(Self {
            lens,
            codes,
            max_bits,
            last,
        })
    }

    /// Whether every byte `counts` holds has a code.
    pub(crate) fn covers(&self, counts: &[u32; 256]) -> bool {
        counts
            .iter()
            .zip(&self.lens)
            .all(|(&c, &l)| c == 0 || l > 0)
    }

    /// The bits `counts` costs under this code.
    pub(crate) fn cost_bits(&self, counts: &[u32; 256]) -> u64 {
        counts
            .iter()
            .zip(&self.lens)
            .map(|(&c, &l)| u64::from(c) * u64::from(l))
            .sum()
    }

    /// Writes the `Huffman_Tree_Description` (RFC 8878 §4.2.1): the weights of bytes
    /// `0..last` (the last byte's is implied), FSE-compressed (§4.2.1.2) or four bits each
    /// (§4.2.1.1), whichever is shorter. `false`, writing nothing, when neither can carry
    /// them (more than 128 weights that FSE cannot shrink below 128 bytes).
    pub(crate) fn write_description(&self, out: &mut Vec<u8>) -> bool {
        let weights: Vec<u8> = self
            .lens
            .iter()
            .take(usize::from(self.last))
            .map(|&l| if l == 0 { 0 } else { self.max_bits + 1 - l })
            .collect();
        let direct = (weights.len() <= 128).then(|| 1 + weights.len().div_ceil(2));
        let mut fse = Vec::new();
        let compressed = compress_weights(&weights, &mut fse);
        match (compressed, direct) {
            (true, Some(d)) if fse.len() + 1 < d => {}
            (true, None) => {}
            (_, Some(_)) => {
                out.push(127 + weights.len() as u8);
                for pair in weights.chunks(2) {
                    let lo = pair.get(1).copied().unwrap_or(0);
                    out.push(pair.first().copied().unwrap_or(0) << 4 | lo);
                }
                return true;
            }
            (false, None) => return false,
        }
        out.push(fse.len() as u8);
        out.extend_from_slice(&fse);
        true
    }

    /// Writes one Huffman-coded stream of `lits` (RFC 8878 §4.2.2): codes from the last
    /// byte to the first, then the end marker, so a decoder reading backward meets the
    /// first byte first.
    pub(crate) fn write_stream(&self, lits: &[u8], out: &mut Vec<u8>) {
        let mut w = BitWriter::new(out);
        for &b in lits.iter().rev() {
            let i = usize::from(b);
            let (code, len) = (
                self.codes.get(i).copied().unwrap_or(0),
                self.lens.get(i).copied().unwrap_or(0),
            );
            w.add(u64::from(code), u32::from(len));
        }
        w.close();
    }
}

/// Weights FSE-compressed into `out` (RFC 8878 §4.2.1.2): a table description, then one
/// bitstream two states share, the first decoding the even weights and the second the odd.
/// `false` when FSE does not apply (two weights or fewer, one weight value only, or every
/// value once) or its bytes would not fit the 7-bit header.
fn compress_weights(weights: &[u8], out: &mut Vec<u8>) -> bool {
    let n = weights.len();
    if n <= 2 {
        return false;
    }
    let mut counts = [0u32; MAX_BITS as usize + 1];
    for &w in weights {
        if let Some(c) = counts.get_mut(usize::from(w)) {
            *c += 1;
        }
    }
    let max_count = counts.iter().copied().max().unwrap_or(0);
    if max_count as usize == n || max_count == 1 {
        return false;
    }
    let max_weight = counts.iter().rposition(|&c| c > 0).unwrap_or(0);
    let counts = counts.get(..=max_weight).unwrap_or_default();
    let log = fse::optimal_log(WEIGHT_LOG, n, max_weight as u32);
    let mut norm = Vec::new();
    fse::normalize(counts, log, &mut norm);
    fse::write_description(&norm, log, out);
    let table = fse::Table::new(&norm, log);
    let mut w = BitWriter::new(out);
    let at = |i: usize| weights.get(i).copied().unwrap_or(0);
    // The last weight of each parity starts its state; the rest go back to front.
    let (mut even, mut odd) = if n % 2 == 1 {
        (table.init(at(n - 1)), table.init(at(n - 2)))
    } else {
        (table.init(at(n - 2)), table.init(at(n - 1)))
    };
    for i in (0..n - 2).rev() {
        let state = if i % 2 == 0 { &mut even } else { &mut odd };
        table.encode(&mut w, state, at(i));
    }
    table.flush(&mut w, odd);
    table.flush(&mut w, even);
    w.close();
    out.len() < 128
}

/// Code lengths for `counts`, none longer than `limit`, forming a complete prefix code;
/// `None` when fewer than two bytes occur.
fn lengths(counts: &[u32; 256], limit: u32) -> Option<[u8; 256]> {
    let mut leaves: Vec<(u32, u8)> = counts
        .iter()
        .enumerate()
        .filter(|&(_, &c)| c > 0)
        .map(|(s, &c)| (c, s as u8))
        .collect();
    if leaves.len() < 2 {
        return None;
    }
    leaves.sort_unstable();
    let n = leaves.len();
    // Two queues: the sorted leaves, and the internal nodes in the (nondecreasing) order
    // they are made.
    let mut weight: Vec<u64> = leaves.iter().map(|&(c, _)| u64::from(c)).collect();
    weight.resize(2 * n - 1, 0);
    let mut parent = vec![0usize; 2 * n - 1];
    let (mut leaf, mut node) = (0usize, n);
    for made in n..2 * n - 1 {
        let mut pick = || {
            let take_leaf = leaf < n
                && (node >= made
                    || weight.get(leaf).copied().unwrap_or(u64::MAX)
                        <= weight.get(node).copied().unwrap_or(u64::MAX));
            if take_leaf {
                leaf += 1;
                leaf - 1
            } else {
                node += 1;
                node - 1
            }
        };
        let (a, b) = (pick(), pick());
        let sum = weight.get(a).copied().unwrap_or(0) + weight.get(b).copied().unwrap_or(0);
        if let Some(w) = weight.get_mut(made) {
            *w = sum;
        }
        for child in [a, b] {
            if let Some(p) = parent.get_mut(child) {
                *p = made;
            }
        }
    }
    let mut depth = vec![0u32; 2 * n - 1];
    for i in (0..2 * n - 2).rev() {
        let d = parent
            .get(i)
            .and_then(|&p| depth.get(p))
            .copied()
            .unwrap_or(0)
            + 1;
        if let Some(slot) = depth.get_mut(i) {
            *slot = d;
        }
    }
    let mut lens: Vec<u32> = depth.get(..n).unwrap_or_default().to_vec();
    limit_lengths(&mut lens, limit);
    let mut out = [0u8; 256];
    for (&(_, s), &l) in leaves.iter().zip(&lens) {
        if let Some(slot) = out.get_mut(usize::from(s)) {
            *slot = l as u8;
        }
    }
    Some(out)
}

/// Caps `lens` (of leaves sorted by ascending count) at `limit` and restores a complete
/// code: lengthen the least frequent codes just under the cap until the Kraft sum fits,
/// then shorten the most frequent of the longest codes while it still fits.
fn limit_lengths(lens: &mut [u32], limit: u32) {
    if lens.iter().all(|&l| l <= limit) {
        return;
    }
    for l in lens.iter_mut() {
        *l = (*l).min(limit);
    }
    let target = 1u64 << limit;
    let mut kraft: u64 = lens.iter().map(|&l| 1u64 << (limit - l)).sum();
    while kraft > target {
        let longest_short = lens.iter().copied().filter(|&l| l < limit).max();
        let Some(at) = longest_short.and_then(|m| lens.iter().position(|&l| l == m)) else {
            break;
        };
        if let Some(l) = lens.get_mut(at) {
            *l += 1;
            kraft -= 1u64 << (limit - *l);
        }
    }
    while kraft < target {
        let longest = lens.iter().copied().max().unwrap_or(1);
        let Some(at) = lens.iter().rposition(|&l| l == longest && l > 1) else {
            break;
        };
        if let Some(l) = lens.get_mut(at) {
            kraft += 1u64 << (limit - *l);
            *l -= 1;
        }
    }
    debug_assert_eq!(kraft, target, "a complete code");
}

#[cfg(test)]
mod tests {
    use super::{Code, MAX_BITS, lengths};
    use crate::decode::huffman::HuffmanTable;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn histogram(bytes: &[u8]) -> [u32; 256] {
        let mut counts = [0u32; 256];
        for &b in bytes {
            counts[usize::from(b)] += 1;
        }
        counts
    }

    /// Writes the description and one stream, then decodes them with the crate's
    /// Huffman decoder.
    fn round_trip(lits: &[u8]) -> Vec<u8> {
        let code = Code::new(&histogram(lits)).expect("two bytes at least");
        let mut desc = Vec::new();
        assert!(code.write_description(&mut desc));
        let mut table = HuffmanTable::new();
        assert_eq!(table.build_decoder(&desc).unwrap(), desc.len());
        let mut stream = Vec::new();
        code.write_stream(lits, &mut stream);
        let mut out = vec![0u8; lits.len()];
        table.decode_streams([&stream[..]], [&mut out[..]]).unwrap();
        out
    }

    #[test]
    fn skewed_flat_and_sparse_alphabets_round_trip() {
        let mut rng = Rng(0xDEAD_BEEF_F00D_0001);
        for case in 0..200u32 {
            let alphabet = 2 + (case * 37) % 255;
            let len = 2 + (rng.next() % 5000) as usize;
            let skew = case % 4;
            let lits: Vec<u8> = (0..len)
                .map(|_| {
                    let mut r = rng.next() % u64::from(alphabet);
                    for _ in 0..skew {
                        r = r.min(rng.next() % u64::from(alphabet));
                    }
                    (r as u8).wrapping_mul(if case % 3 == 0 { 7 } else { 1 })
                })
                .collect();
            if histogram(&lits).iter().filter(|&&c| c > 0).count() < 2 {
                continue;
            }
            assert_eq!(round_trip(&lits), lits, "case {case}");
        }
    }

    /// Fibonacci counts, whose Huffman code reaches 23 bits, are cut to 11 bits and still
    /// form a complete code that decodes.
    #[test]
    fn deep_trees_are_cut_to_eleven_bits() {
        let mut counts = [0u32; 256];
        let (mut a, mut b) = (1u32, 1u32);
        for c in counts.iter_mut().take(24) {
            *c = a;
            (a, b) = (b, a + b);
        }
        let lens = lengths(&counts, MAX_BITS).unwrap();
        assert!(lens.iter().all(|&l| u32::from(l) <= MAX_BITS));
        let kraft: u64 = lens
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 1u64 << (MAX_BITS - u32::from(l)))
            .sum();
        assert_eq!(kraft, 1 << MAX_BITS);
        let lits: Vec<u8> = counts
            .iter()
            .enumerate()
            .flat_map(|(s, &c)| std::iter::repeat_n(s as u8, c.min(3000) as usize))
            .collect();
        assert_eq!(round_trip(&lits), lits);
    }

    /// Every byte present with similar counts: 255 weights, more than the four-bit form
    /// holds, so the FSE form carries them.
    #[test]
    fn all_256_bytes_need_the_fse_weight_form() {
        let mut rng = Rng(5);
        let lits: Vec<u8> = (0..20_000)
            .map(|i| {
                if i % 3 == 0 {
                    (rng.next() % 16) as u8
                } else {
                    rng.next() as u8
                }
            })
            .collect();
        let code = Code::new(&histogram(&lits)).unwrap();
        let mut desc = Vec::new();
        assert!(code.write_description(&mut desc));
        assert!(desc[0] < 128, "FSE-compressed weights");
        assert_eq!(round_trip(&lits), lits);
    }

    /// The negative control: a stream decoded under another code's description is not
    /// the literals.
    #[test]
    fn a_stream_under_another_description_differs() {
        let a: Vec<u8> = b"aaaaaaaabbbbccd".repeat(20);
        let b: Vec<u8> = b"abbbbbbbbccccdd".repeat(20);
        let code_a = Code::new(&histogram(&a)).unwrap();
        let code_b = Code::new(&histogram(&b)).unwrap();
        let mut desc = Vec::new();
        assert!(code_b.write_description(&mut desc));
        let mut table = HuffmanTable::new();
        table.build_decoder(&desc).unwrap();
        let mut stream = Vec::new();
        code_a.write_stream(&a, &mut stream);
        let mut out = vec![0u8; a.len()];
        let decoded = table.decode_streams([&stream[..]], [&mut out[..]]);
        assert!(decoded.is_err() || out != a);
    }
}
