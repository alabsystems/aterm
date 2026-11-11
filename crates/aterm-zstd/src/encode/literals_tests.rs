// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::write;
use crate::decode::huffman::HuffmanTable;
use crate::decode::literals;
use crate::encode::huffman::Code;

/// The section decoded by the crate's literals decoder, the table carried across.
fn decode(section: &[u8], table: &mut HuffmanTable) -> (Vec<u8>, usize) {
    let mut padded = section.to_vec();
    padded.resize(section.len() + 64, 0);
    let mut buf = Vec::new();
    let (lits, count, used) =
        literals::decode(&padded, section.len(), table, &mut buf, 128 << 10).unwrap();
    (lits[..count].to_vec(), used)
}

fn text(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            b"etaoin shrdlu,.\n"[(x % 16) as usize] ^ if x.is_multiple_of(97) { 0x20 } else { 0 }
        })
        .collect()
}

/// Every section kind and header width: each decodes to its literals, and a code a
/// compressed section described is what a later treeless one reuses.
#[test]
fn every_kind_and_width_decodes() {
    let mut table = HuffmanTable::new();
    let mut prev: Option<Code> = None;
    let mut kinds = [0usize; 4];
    let mut inputs: Vec<Vec<u8>> = vec![Vec::new(), b"x".to_vec(), vec![7; 31], vec![7; 32]];
    inputs.extend([vec![9; 4095], vec![9; 4096], vec![9; 100_000]]);
    for len in [
        8,
        31,
        32,
        200,
        255,
        256,
        1000,
        1023,
        1024,
        5000,
        16_383,
        16_384,
        128 << 10,
    ] {
        inputs.push(text(len, len as u64));
        inputs.push(text(len, len as u64 + 1));
    }
    let noise: Vec<u8> = (0..3000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    inputs.push(noise);
    for lits in &inputs {
        let mut section = Vec::new();
        let fresh = write(lits, prev.as_ref(), &mut section);
        kinds[usize::from(section[0] & 3)] += 1;
        let (back, used) = decode(&section, &mut table);
        assert_eq!(
            used,
            section.len(),
            "{} literals: section length",
            lits.len()
        );
        assert!(back == *lits, "{} literals", lits.len());
        if fresh.is_some() {
            prev = fresh;
        }
    }
    assert!(
        kinds.iter().all(|&k| k > 0),
        "raw, RLE, compressed and treeless: {kinds:?}"
    );
}

/// The negative control: a treeless section read with no table is refused.
#[test]
fn a_treeless_section_without_a_table_is_refused() {
    let lits = text(5000, 3);
    let mut first = Vec::new();
    let code = write(&lits, None, &mut first).expect("a new code");
    let mut second = Vec::new();
    assert!(write(&lits, Some(&code), &mut second).is_none());
    assert_eq!(second[0] & 3, 3, "treeless");
    let mut padded = second.clone();
    padded.resize(second.len() + 64, 0);
    let mut buf = Vec::new();
    let mut fresh = HuffmanTable::new();
    assert!(literals::decode(&padded, second.len(), &mut fresh, &mut buf, 128 << 10).is_err());
}
