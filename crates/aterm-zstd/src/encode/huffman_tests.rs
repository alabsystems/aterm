// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
