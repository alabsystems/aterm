// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Matcher, Search, common};

const SEARCH: Search = Search {
    hash_log: 12,
    chain_log: 12,
    depth: 16,
    lazy: 2,
    target: 64,
};

/// The sequences tile the block and every match copies equal bytes from inside the
/// window.
fn check(h: &[u8], start: usize, end: usize, max_dist: usize, seqs: &[super::Seq]) {
    let mut at = start;
    for s in seqs {
        at += s.lit_len as usize;
        let (len, d) = (s.match_len as usize, s.offset as usize);
        assert!(d >= 1 && d <= max_dist && d <= at, "distance {d} at {at}");
        assert!(len >= 3 && at + len <= end);
        for k in 0..len {
            assert_eq!(h[at + k], h[at + k - d], "match byte {k} at {at}");
        }
        at += len;
    }
    assert!(at <= end);
}

#[test]
fn matches_copy_equal_bytes_within_the_window() {
    let mut h = Vec::new();
    let mut x = 7u64;
    for i in 0..200_000usize {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        h.push(if i % 1000 < 600 {
            b"abcdefgh"[(x % 8) as usize]
        } else {
            (x >> 9) as u8
        });
    }
    for max_dist in [1023, 4095, 1 << 20] {
        let mut m = Matcher::new(SEARCH, 1);
        let mut seqs = Vec::new();
        let mut start = 0;
        while start < h.len() {
            let end = (start + 50_000).min(h.len());
            m.find(&h, 1, start, end, max_dist, &mut seqs);
            assert!(!seqs.is_empty());
            check(&h, start, end, max_dist, &seqs);
            start = end;
        }
    }
}

/// The negative control: the checker refuses a match that copies other bytes.
#[test]
#[should_panic(expected = "match byte")]
fn a_wrong_match_is_caught() {
    let h = b"abcdefghabcdefgh".to_vec();
    check(
        &h,
        0,
        16,
        64,
        &[super::Seq {
            lit_len: 9,
            match_len: 4,
            offset: 7,
        }],
    );
}

#[test]
fn common_prefix_lengths() {
    let h = b"0123456789abcdef0123456789abcdeX".to_vec();
    assert_eq!(common(&h, 0, 16, 16), 15);
    assert_eq!(common(&h, 0, 16, 9), 9);
    assert_eq!(common(&h, 1, 16, 8), 0);
}
