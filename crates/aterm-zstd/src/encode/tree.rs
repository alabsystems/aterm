// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The binary-tree match finder the optimal parser searches: every position of the
//! window sorted by the bytes that follow it, so one walk from the newest position with
//! the same first bytes meets the longest matches at each length.
//!
//! Positions are virtual, as in [`super::matcher`]: a buffer index plus the base of the
//! encoder's history, 0 marking an empty slot. Node `p` lives in slot `p & mask` of a ring
//! of `1 << tree_log` nodes, each a pair of links: the newest older position whose bytes
//! sort below `p`'s, and the newest that sorts above. A walk stops at a node old enough
//! that the ring may have reused its slot.
//!
//! The tree is only ever ordered by bytes that differ: a comparison that reaches the end
//! of the block before the bytes differ stops the walk there, and the walk drops the rest
//! of the old tree rather than guess. So two nodes on either side of a walk share at least
//! the shorter of their common prefixes with the position inserted, which is where the
//! next comparison starts.

use super::matcher::common;

/// A match the tree or a repeat offset gives at a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cand {
    /// Its length.
    pub(crate) len: u32,
    /// Its `Offset_Value` (RFC 8878 §3.1.1.5): 1 to 3 a repeat offset (read under the
    /// position's literal length), else the distance plus 3.
    pub(crate) off: u32,
}

/// Tree parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TreeParams {
    /// The hash of the first `min_match` bytes picks a tree.
    pub(crate) hash_log: u32,
    /// The ring holds this many positions (log 2).
    pub(crate) tree_log: u32,
    /// Nodes compared per search.
    pub(crate) depth: u32,
    /// 3 or 4: the bytes the hash covers and the shortest match reported.
    pub(crate) min_match: u32,
    /// A match longer than this is taken at once (the parser's `sufficient_len`).
    pub(crate) target: u32,
}

/// A match longer than this ends a search: the parser's horizon is no longer.
pub(crate) const LONGEST_SEARCHED: usize = 1 << 12;

/// The block a search runs in.
pub(crate) struct Ctx<'a> {
    pub(crate) h: &'a [u8],
    /// The virtual position of `h[0]`.
    pub(crate) base: u32,
    /// Matches and comparisons end here (the block's end).
    pub(crate) end: usize,
    /// The farthest a match may reach.
    pub(crate) max_dist: usize,
}

impl Ctx<'_> {
    /// The lowest position a match from `cur` may start at: inside the buffer and the
    /// window, never 0.
    #[inline]
    fn low(&self, cur: u32) -> u32 {
        self.base
            .max(cur.saturating_sub(self.max_dist as u32))
            .max(1)
    }

    /// The match length at `ip` from `dist` back when its first `min` bytes agree, else 0.
    #[inline]
    fn rep_len(&self, ip: usize, dist: u32, min: usize) -> u32 {
        let d = dist as usize;
        if d == 0 || d > ip || d > self.max_dist {
            return 0;
        }
        let max = self.end.saturating_sub(ip);
        if max < min {
            return 0;
        }
        let len = common(self.h, ip - d, ip, max);
        if len >= min { len as u32 } else { 0 }
    }
}

#[derive(Debug)]
pub(crate) struct Tree {
    p: TreeParams,
    hash: Vec<u32>,
    nodes: Vec<u32>,
    mask: u32,
    /// The first position not yet inserted (positions inside a long repeat are skipped).
    next: u32,
}

/// Where a walk writes its next link: a node's slot, or nowhere once it left the tree.
const NOWHERE: usize = usize::MAX;

impl Tree {
    /// A tree whose first position is `start`.
    pub(crate) fn new(p: TreeParams, start: u32) -> Self {
        Self {
            p,
            hash: vec![0; 1 << p.hash_log],
            nodes: vec![0; 2 << p.tree_log],
            mask: (1 << p.tree_log) - 1,
            next: start,
        }
    }

    /// The ring's length: a rebase must move positions by a multiple of it.
    pub(crate) const fn ring_len(&self) -> u32 {
        self.mask + 1
    }

    /// Moves every position down by `by` (a multiple of [`Self::ring_len`]), forgetting
    /// those below `keep_from`.
    pub(crate) fn rebase(&mut self, by: u32, keep_from: u32) {
        debug_assert_eq!(by % self.ring_len(), 0);
        let shift = |v: &mut u32| *v = if *v >= keep_from { *v - by } else { 0 };
        self.hash.iter_mut().for_each(shift);
        self.nodes.iter_mut().for_each(shift);
        self.next = self.next.saturating_sub(by);
    }

    /// The hash of the bytes at `h[i..]` (`min_match` of them).
    #[inline]
    fn slot(&self, h: &[u8], i: usize) -> usize {
        let word = h
            .get(i..i + 4)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .map(u32::from_le_bytes)
            .or_else(|| {
                h.get(i..i + 3)
                    .map(|s| s.iter().rev().fold(0u32, |w, &b| w << 8 | u32::from(b)))
            })
            .unwrap_or(0);
        let word = if self.p.min_match == 3 {
            word << 8
        } else {
            word
        };
        (word.wrapping_mul(0x9E37_79B1) >> (32 - self.p.hash_log)) as usize
    }

    /// Inserts every position before `ip` not yet in the tree.
    pub(crate) fn insert_until(&mut self, at: &Ctx<'_>, ip: usize) {
        let target = at.base + ip as u32;
        let mut v = self.next.max(at.base);
        while v < target {
            let forward = self.insert(at, (v - at.base) as usize, target);
            v += forward.max(1);
        }
        self.next = self.next.max(target);
    }

    /// Inserts position `ip` (not searching for matches); how many positions on the next
    /// insertion may start (more than one inside a long repeat).
    fn insert(&mut self, at: &Ctx<'_>, ip: usize, target: u32) -> u32 {
        let h = at.h;
        let cur = at.base + ip as u32;
        let slot = self.slot(h, ip);
        let Some(head) = self.hash.get_mut(slot) else {
            return 1;
        };
        let mut m = *head;
        *head = cur;
        let limit = at.end.saturating_sub(ip);
        // The window seen from the end of this round of insertions.
        let low = at.low(target);
        let tree_low = cur.saturating_sub(self.mask);
        let mut smaller = 2 * (cur & self.mask) as usize;
        let mut larger = smaller + 1;
        let (mut common_smaller, mut common_larger) = (0usize, 0usize);
        let mut best = 8usize;
        let mut match_end = cur + 9;
        let mut tries = self.p.depth;
        while tries > 0 && m >= low {
            tries -= 1;
            let mi = (m - at.base) as usize;
            let next = 2 * (m & self.mask) as usize;
            let known = common_smaller.min(common_larger);
            let len = known + common(h, mi + known, ip + known, limit - known);
            if len > best {
                best = len;
                match_end = match_end.max(m + len as u32);
            }
            if len >= limit {
                break;
            }
            if h.get(mi + len) < h.get(ip + len) {
                self.link(smaller, m);
                common_smaller = len;
                if m <= tree_low {
                    smaller = NOWHERE;
                    break;
                }
                smaller = next + 1;
                m = self.nodes.get(next + 1).copied().unwrap_or(0);
            } else {
                self.link(larger, m);
                common_larger = len;
                if m <= tree_low {
                    larger = NOWHERE;
                    break;
                }
                larger = next;
                m = self.nodes.get(next).copied().unwrap_or(0);
            }
        }
        self.link(smaller, 0);
        self.link(larger, 0);
        let long = if best > 384 {
            (best as u32 - 384).min(192)
        } else {
            0
        };
        long.max(match_end - (cur + 8))
    }

    #[inline]
    fn link(&mut self, at: usize, to: u32) {
        if let Some(n) = self.nodes.get_mut(at) {
            *n = to;
        }
    }

    /// Every match at `ip` longer than the one before it, into `out` (shortest first):
    /// the repeat offsets `reps` (read as a position after `lit_len == 0` reads them when
    /// `ll0`), then the tree, which `ip` then joins. Nothing at a position inside a long
    /// repeat an earlier search skipped.
    pub(crate) fn matches(
        &mut self,
        at: &Ctx<'_>,
        ip: usize,
        reps: &[u32; 3],
        ll0: bool,
        out: &mut Vec<Cand>,
    ) {
        out.clear();
        let cur = at.base + ip as u32;
        if cur < self.next {
            return;
        }
        self.insert_until(at, ip);
        let h = at.h;
        let min = self.p.min_match as usize;
        let limit = at.end.saturating_sub(ip);
        if limit < min {
            return;
        }
        let target = self.p.target as usize;
        let mut best = min - 1;
        // Repeat offsets: with no literal before the match, the first names the second
        // repeat offset, the second the third, and the third the first less one.
        let first = usize::from(ll0);
        for code in first..first + 3 {
            let dist = match code {
                0 => reps[0],
                1 => reps[1],
                2 => reps[2],
                _ => reps[0].wrapping_sub(1),
            };
            let len = at.rep_len(ip, dist, min) as usize;
            if len > best {
                best = len;
                out.push(Cand {
                    len: len as u32,
                    off: (code - first + 1) as u32,
                });
                if len > target || len == limit {
                    // The tree is not walked: `ip` joins it on a later insertion.
                    return;
                }
            }
        }

        let slot = self.slot(h, ip);
        let Some(head) = self.hash.get_mut(slot) else {
            return;
        };
        let mut m = *head;
        *head = cur;
        let low = at.low(cur);
        let tree_low = cur.saturating_sub(self.mask);
        let mut smaller = 2 * (cur & self.mask) as usize;
        let mut larger = smaller + 1;
        let (mut common_smaller, mut common_larger) = (0usize, 0usize);
        let mut match_end = cur + 9;
        let mut tries = self.p.depth;
        while tries > 0 && m >= low {
            tries -= 1;
            let mi = (m - at.base) as usize;
            let next = 2 * (m & self.mask) as usize;
            let known = common_smaller.min(common_larger);
            debug_assert!(
                h.get(mi..mi + known) == h.get(ip..ip + known),
                "the tree's guaranteed prefix holds"
            );
            let len = known + common(h, mi + known, ip + known, limit - known);
            if len > best {
                match_end = match_end.max(m + len as u32);
                best = len;
                out.push(Cand {
                    len: len as u32,
                    off: (ip - mi) as u32 + 3,
                });
                if len > LONGEST_SEARCHED || len >= limit {
                    break;
                }
            }
            if len >= limit {
                break;
            }
            if h.get(mi + len) < h.get(ip + len) {
                self.link(smaller, m);
                common_smaller = len;
                if m <= tree_low {
                    smaller = NOWHERE;
                    break;
                }
                smaller = next + 1;
                m = self.nodes.get(next + 1).copied().unwrap_or(0);
            } else {
                self.link(larger, m);
                common_larger = len;
                if m <= tree_low {
                    larger = NOWHERE;
                    break;
                }
                larger = next;
                m = self.nodes.get(next).copied().unwrap_or(0);
            }
        }
        self.link(smaller, 0);
        self.link(larger, 0);
        // Inside a long repeat the next positions are skipped.
        self.next = match_end - 8;
    }
}

#[cfg(test)]
mod tests {
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
}
