// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::io::Read as _;

use super::{BlockEncoder, COMPRESSED_BLOCK, RLE_BLOCK};
use crate::encode::opt::{OptParams, Parser};
use crate::encode::tree::TreeParams;
use crate::frame::Checksum;

fn xorshift(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed | 1;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// A block of two kinds: 64 KiB of text from a few words, which matches find, then
/// 64 KiB of bytes from a 16-letter alphabet, mostly literals.
fn two_kinds() -> Vec<u8> {
    const WORDS: [&[u8]; 6] = [b"frame ", b"block ", b"split ", b"part ", b"cost ", b"\n"];
    let mut next = xorshift(5);
    let mut h = Vec::new();
    while h.len() < 64 << 10 {
        let x = next();
        h.extend_from_slice(WORDS[(x % 6) as usize]);
        if x.is_multiple_of(5) {
            h.extend_from_slice(&x.to_le_bytes()[..3]);
        }
    }
    h.truncate(64 << 10);
    while h.len() < 128 << 10 {
        h.push(0x80 | (next() >> 60) as u8);
    }
    h
}

/// The blocks `write` appended: each one's type, and whether it is the last.
fn kinds(mut blocks: &[u8]) -> Vec<(u32, bool)> {
    let mut out = Vec::new();
    while let [a, b, c, rest @ ..] = blocks {
        let word = u32::from_le_bytes([*a, *b, *c, 0]);
        let (kind, size) = ((word >> 1) & 3, (word >> 3) as usize);
        out.push((kind, word & 1 == 1));
        blocks = rest
            .get(if kind == RLE_BLOCK { 1 } else { size }..)
            .unwrap_or_default();
    }
    out
}

/// `blocks` after a single-segment header for `len` bytes, decoded.
fn decoded(blocks: &[u8], len: usize) -> Vec<u8> {
    let mut zst = Vec::new();
    crate::encode::write_header(&mut zst, 17, Some(len as u64), true, Checksum::Omitted);
    zst.extend_from_slice(blocks);
    let mut out = Vec::new();
    crate::Decoder::with_buffer(&zst[..])
        .read_to_end(&mut out)
        .expect("a frame of the blocks");
    out
}

/// Cut where its halves cost less, the block of two kinds goes out as several blocks,
/// smaller than it is whole, and decodes to the same bytes; the control, not split, is
/// one block.
#[test]
fn a_block_of_two_kinds_is_split_where_its_parts_cost_less() {
    let h = two_kinds();
    let params = OptParams {
        tree: TreeParams {
            hash_log: 16,
            tree_log: 16,
            depth: 32,
            min_match: 3,
            target: 256,
        },
        ultra: true,
        two_pass: false,
        ldm: None,
    };
    let mut seqs = Vec::new();
    Parser::new(params, 1).find(&h, 1, 0, h.len(), 1 << 17, [1, 4, 8], &mut seqs);
    assert!(
        seqs.len() >= 2 * super::SPLIT_MIN,
        "{} sequences",
        seqs.len()
    );
    let (mut split, mut whole) = (Vec::new(), Vec::new());
    BlockEncoder::new().write(&h, Some(&seqs), true, true, &mut split);
    BlockEncoder::new().write(&h, Some(&seqs), false, true, &mut whole);
    let parts = kinds(&split);
    assert!(parts.len() >= 2, "{parts:?}");
    assert!(
        parts.iter().all(|&(k, _)| k == COMPRESSED_BLOCK)
            && parts.iter().filter(|p| p.1).count() == 1
            && parts.last().is_some_and(|p| p.1),
        "{parts:?}: compressed parts, the last one last"
    );
    assert_eq!(kinds(&whole), [(COMPRESSED_BLOCK, true)]);
    assert!(
        split.len() < whole.len(),
        "{} vs {} bytes",
        split.len(),
        whole.len()
    );
    assert!(decoded(&split, h.len()) == h);
    assert!(decoded(&whole, h.len()) == h);
}
