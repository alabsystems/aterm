// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Ldm, LdmParams, LongMatch};

/// Room for an anchor per 16 bytes of 8 MiB, as a 2^23 window's table would have.
const P: LdmParams = LdmParams {
    table_log: 19,
    bucket_log: 3,
    min_len: 32,
    rate_log: 4,
};

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// The matches are real, apart and in order, each inside the window and its block.
fn check(h: &[u8], start: usize, end: usize, max_dist: usize, ms: &[LongMatch]) {
    let mut after = start;
    for m in ms {
        let d = m.dist as usize;
        assert!(
            m.start >= after && m.end <= end,
            "{m:?} in order, in the block"
        );
        assert!(d >= 1 && d <= m.start && d <= max_dist, "{m:?}: distance");
        assert!(
            h[m.start..m.end] == h[m.start - d..m.end - d],
            "{m:?}: match bytes"
        );
        after = m.end;
    }
}

/// A 40 KiB stretch of noise copied 3 MiB on, between other noise: found whole, block
/// by block, at its distance, and nothing else is.
#[test]
fn a_far_copy_is_found_block_by_block() {
    let mut h = noise(4 << 20, 1);
    let (from, to, len) = (100_000, 100_000 + (3 << 20), 40_000);
    h.copy_within(from..from + len, to);
    let mut ldm = Ldm::new(P);
    let mut out = Vec::new();
    let mut covered = 0;
    let mut start = 0;
    while start < h.len() {
        let end = (start + (128 << 10)).min(h.len());
        ldm.find(&h, 1, start, end, 1 << 27, &mut out);
        check(&h, start, end, 1 << 27, &out);
        for m in &out {
            assert_eq!(m.dist as usize, to - from);
            covered += m.end - m.start;
        }
        start = end;
    }
    assert!(covered > len - 64, "{covered} of {len} bytes");
}

/// The negative control: past the window the copy is not found.
#[test]
fn a_copy_past_the_window_is_not() {
    let mut h = noise(4 << 20, 2);
    h.copy_within(100_000..140_000, 100_000 + (3 << 20));
    let mut ldm = Ldm::new(P);
    let mut out = Vec::new();
    let mut found = 0;
    let mut start = 0;
    while start < h.len() {
        let end = (start + (128 << 10)).min(h.len());
        ldm.find(&h, 1, start, end, 1 << 21, &mut out);
        found += out.len();
        start = end;
    }
    assert_eq!(found, 0);
}
