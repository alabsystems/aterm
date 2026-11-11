// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Cand, Ctx, Tree, TreeParams};

const P: TreeParams = TreeParams {
    hash_log: 12,
    tree_log: 12,
    depth: 64,
    min_match: 3,
    target: 4096,
};

fn text(len: usize) -> Vec<u8> {
    let mut x = 0x5EED_u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            b"abcab\0\0xyz"[(x % 10) as usize]
        })
        .collect()
}

/// Each match copies equal bytes from inside the window, and each is longer than the
/// one before it.
fn check(h: &[u8], ip: usize, reps: &[u32; 3], max_dist: usize, out: &[Cand]) {
    let mut last = 0;
    for c in out {
        assert!(c.len > last, "lengths grow at {ip}");
        last = c.len;
        let d = match c.off {
            1..=3 => reps[c.off as usize - 1] as usize,
            o => o as usize - 3,
        };
        assert!(d >= 1 && d <= ip && d <= max_dist, "distance {d} at {ip}");
        let len = c.len as usize;
        assert!(
            h[ip..ip + len] == h[ip - d..ip - d + len],
            "match bytes at {ip}"
        );
    }
}

/// Every match the tree reports is real, inside the window and the ring, at three
/// window and ring sizes; and it finds plenty.
#[test]
fn matches_are_real_and_grow() {
    let h = text(60_000);
    for (max_dist, tree_log) in [(1 << 20, 16), (999, 16), (1 << 20, 10)] {
        let mut tree = Tree::new(TreeParams { tree_log, ..P }, 1);
        let mut out = Vec::new();
        let at = Ctx {
            h: &h,
            base: 1,
            end: h.len(),
            max_dist,
        };
        let reps = [1, 4, 8];
        let mut found = 0;
        for ip in 0..h.len() - 8 {
            tree.matches(&at, ip, &reps, false, &mut out);
            check(&h, ip, &reps, max_dist, &out);
            found += out.len();
        }
        assert!(found > 10_000, "{found} matches");
    }
}

/// The negative control: the checker refuses a match that copies other bytes.
#[test]
#[should_panic(expected = "match bytes")]
fn a_wrong_match_is_caught() {
    let h = b"abcdefabcxefabcdef".to_vec();
    check(&h, 6, &[1, 4, 8], 64, &[Cand { len: 4, off: 6 + 3 }]);
}
