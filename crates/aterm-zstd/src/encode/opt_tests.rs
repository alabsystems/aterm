// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{OptParams, Parser, rescale, weight};
use crate::encode::ldm::LdmParams;
use crate::encode::tree::TreeParams;

const P: OptParams = OptParams {
    tree: TreeParams {
        hash_log: 14,
        tree_log: 14,
        depth: 32,
        min_match: 3,
        target: 256,
    },
    ultra: true,
    two_pass: true,
    ldm: None,
};

fn text(len: usize, seed: u64) -> Vec<u8> {
    const WORDS: [&[u8]; 8] = [
        b"price ",
        b"parse ",
        b"path ",
        b"stretch ",
        b"literal ",
        b"match ",
        b"\n",
        b"\0\0\0\0",
    ];
    let mut x = seed | 1;
    let mut out = Vec::new();
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(WORDS[(x % 8) as usize]);
        if x.is_multiple_of(13) {
            out.push((x >> 40) as u8);
        }
    }
    out.truncate(len);
    out
}

/// `h` parsed block by block: each match as `(at, len, distance)`, held to tile a prefix
/// of its block and to copy equal bytes from inside the window.
fn parsed(params: OptParams, h: &[u8], max_dist: usize) -> Vec<(usize, usize, usize)> {
    let mut p = Parser::new(params, 1);
    let mut seqs = Vec::new();
    let mut matches = Vec::new();
    let mut start = 0;
    while start < h.len() {
        let end = (start + (128 << 10)).min(h.len());
        p.find(h, 1, start, end, max_dist, [1, 4, 8], &mut seqs);
        let mut at = start;
        for s in &seqs {
            at += s.lit_len as usize;
            let (len, d) = (s.match_len as usize, s.offset as usize);
            assert!(len >= 3 && at + len <= end, "match of {len} at {at}");
            assert!(d >= 1 && d <= at && d <= max_dist, "distance {d} at {at}");
            assert!(h[at..at + len] == h[at - d..at - d + len], "bytes at {at}");
            matches.push((at, len, d));
            at += len;
        }
        assert!(at <= end);
        start = end;
    }
    matches
}

/// The sequences tile a prefix of the block and every match copies equal bytes from
/// inside the window, its distance named through the decoder's repeat offsets.
#[test]
fn a_parse_tiles_the_block_with_real_matches() {
    let h = text(300_000, 9);
    for (params, max_dist) in [
        (P, 1 << 20),
        (
            OptParams {
                ultra: false,
                two_pass: false,
                ..P
            },
            4000,
        ),
    ] {
        let covered: usize = parsed(params, &h, max_dist).iter().map(|m| m.1).sum();
        assert!(covered > h.len() / 2, "matches cover {covered} bytes");
    }
}

/// A copy 400 kB back, past the tree's 16 Ki positions, is parsed as matches at its
/// distance when the anchors offer it; the control, without them, never reaches it.
#[test]
fn the_anchors_carry_a_copy_past_the_tree_into_the_parse() {
    let mut x = 7u64;
    let mut h: Vec<u8> = (0..600_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    let (from, to, len) = (50_000, 450_000, 40_000);
    h.copy_within(from..from + len, to);
    let far = |params| -> usize {
        parsed(params, &h, 1 << 20)
            .iter()
            .filter(|m| m.2 == to - from)
            .map(|m| m.1)
            .sum()
    };
    let anchors = Some(LdmParams {
        table_log: 16,
        bucket_log: 3,
        min_len: 32,
        rate_log: 4,
    });
    let found = far(OptParams { ldm: anchors, ..P });
    assert!(
        found > len - 64,
        "{found} of {len} bytes at the copy's distance"
    );
    assert_eq!(far(P), 0, "the tree alone reaches the copy");
}

#[test]
fn weights_and_scaling() {
    assert_eq!(weight(0, false), 0);
    assert_eq!(weight(1, false), 256);
    assert_eq!(weight(0, true), 256);
    assert!(weight(1000, true) > weight(900, true));
    assert_eq!(weight(1000, false), weight(900, false));
    let mut t = [5000u32, 3, 0, 1];
    let sum = rescale(&mut t, 11);
    assert_eq!(sum, t.iter().sum::<u32>());
    assert!(t.iter().all(|&c| c >= 1) && t[0] < 5000);
}
