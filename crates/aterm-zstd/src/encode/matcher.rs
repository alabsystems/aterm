// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The match finder: hash chains over the window, searched greedily or lazily.
//!
//! Positions are "virtual": a buffer index plus the base the encoder's history buffer
//! carries, so sliding the buffer leaves the tables valid. 0 is never a position, so it
//! marks an empty slot. A chain is a ring of `1 << chain_log` links indexed by position,
//! so a link older than the ring's length may have been overwritten and is never
//! followed.

use super::sequences::Seq;

/// The shortest match this finder reports: hashes cover four bytes.
const MIN_MATCH: usize = 4;

/// In a run with no match the search steps further each time, one more byte per
/// `1 << SKIP_LOG` literals since the last match (it spends less time in data that does
/// not compress).
const SKIP_LOG: u32 = 8;

/// Search parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Search {
    pub(crate) hash_log: u32,
    pub(crate) chain_log: u32,
    /// Chain links followed per search.
    pub(crate) depth: u32,
    /// 0 takes the best match at the first position that has one; 1 or 2 also try the next
    /// one or two positions.
    pub(crate) lazy: u8,
    /// A match this long ends the search and the lazy steps.
    pub(crate) target: usize,
}

#[derive(Debug)]
pub(crate) struct Matcher {
    search: Search,
    hash: Vec<u32>,
    chain: Vec<u32>,
    chain_mask: u32,
    /// The first position not yet inserted.
    next: u32,
    /// The two most recent distances, tried first at each position.
    reps: [u32; 2],
}

impl Matcher {
    /// A finder whose first position is `start`.
    pub(crate) fn new(search: Search, start: u32) -> Self {
        Self {
            search,
            hash: vec![0; 1 << search.hash_log],
            chain: vec![0; 1 << search.chain_log],
            chain_mask: (1 << search.chain_log) - 1,
            next: start,
            reps: [1, 4],
        }
    }

    /// The ring's length: a rebase must move positions by a multiple of it.
    pub(crate) const fn chain_len(&self) -> u32 {
        self.chain_mask + 1
    }

    /// Moves every position down by `by` (a multiple of [`Self::chain_len`]), forgetting
    /// those below `keep_from` (older than the buffer holds).
    pub(crate) fn rebase(&mut self, by: u32, keep_from: u32) {
        debug_assert_eq!(by % self.chain_len(), 0);
        let shift = |v: &mut u32| *v = if *v >= keep_from { *v - by } else { 0 };
        self.hash.iter_mut().for_each(shift);
        self.chain.iter_mut().for_each(shift);
        self.next = self.next.saturating_sub(by);
    }

    /// The sequences of block `h[start..end]`, into `seqs`: matches at most `max_dist`
    /// back, each ending inside the block, with the literals before each. `h[..start]`
    /// is history; `base` is the position of `h[0]`.
    pub(crate) fn find(
        &mut self,
        h: &[u8],
        base: u32,
        start: usize,
        end: usize,
        max_dist: usize,
        seqs: &mut Vec<Seq>,
    ) {
        seqs.clear();
        let mut anchor = start;
        let mut ip = start;
        let [mut rep0, mut rep1] = self.reps;
        let lazy = self.search.lazy;
        let target = self.search.target;
        let at = Ctx {
            h,
            base,
            end,
            max_dist,
        };
        while ip + MIN_MATCH <= end {
            let mut best = self.best_at(&at, ip, [rep0, rep1]);
            if best.len < MIN_MATCH {
                ip += 1 + ((ip - anchor) >> SKIP_LOG);
                continue;
            }
            let mut from = ip;
            // Lazy steps: a better match one or two bytes on wins the literal.
            if lazy > 0 {
                'lazy: while best.len < target {
                    for step in 1..=usize::from(lazy) {
                        let p = from + step;
                        if p + MIN_MATCH > end {
                            break 'lazy;
                        }
                        let next = self.best_at(&at, p, [rep0, rep1]);
                        if next.len >= MIN_MATCH
                            && next.gain() > best.gain() + 4 + 3 * (step - 1) as i64
                        {
                            best = next;
                            from = p;
                            continue 'lazy;
                        }
                    }
                    break;
                }
            }
            // Back over equal bytes before the match.
            while from > anchor
                && from > best.dist
                && h.get(from - 1) == h.get(from - 1 - best.dist)
            {
                from -= 1;
                best.len += 1;
            }
            seqs.push(Seq {
                lit_len: (from - anchor) as u32,
                match_len: best.len as u32,
                offset: best.dist as u32,
            });
            let d = best.dist as u32;
            if d != rep0 {
                rep1 = rep0;
                rep0 = d;
            }
            ip = from + best.len;
            anchor = ip;
            // A match at the second most recent distance right after: no literal, and
            // the cheapest offset there is.
            while ip + MIN_MATCH <= end {
                let len = at.len_at(ip, rep1 as usize);
                if len < MIN_MATCH {
                    break;
                }
                seqs.push(Seq {
                    lit_len: 0,
                    match_len: len as u32,
                    offset: rep1,
                });
                (rep0, rep1) = (rep1, rep0);
                ip += len;
                anchor = ip;
            }
        }
        self.reps = [rep0, rep1];
    }

    /// The best match at `ip`: the repeat distances, then the hash chain.
    fn best_at(&mut self, at: &Ctx<'_>, ip: usize, reps: [u32; 2]) -> Match {
        let mut best = Match::NONE;
        for r in reps {
            let len = at.len_at(ip, r as usize);
            if len >= MIN_MATCH && len > best.len {
                best = Match {
                    len,
                    dist: r as usize,
                    rep: true,
                };
            }
        }
        if best.len >= self.search.target || ip + best.len >= at.end {
            self.insert_until(at, ip);
            return best;
        }
        let found = self.search_chain(at, ip, best.len.max(MIN_MATCH - 1));
        if found.len > best.len && found.gain() > best.gain() {
            found
        } else {
            best
        }
    }

    /// Inserts every position before `ip` not yet in the tables.
    fn insert_until(&mut self, at: &Ctx<'_>, ip: usize) {
        let target = at.base + ip as u32;
        let mut v = self.next.max(at.base);
        while v < target {
            let i = (v - at.base) as usize;
            let slot = hash4(at.h, i, self.search.hash_log);
            if let (Some(head), Some(link)) = (
                self.hash.get_mut(slot),
                self.chain.get_mut((v & self.chain_mask) as usize),
            ) {
                *link = *head;
                *head = v;
            }
            v += 1;
        }
        self.next = self.next.max(target);
    }

    /// The longest match at `ip` along its hash chain that beats `floor` bytes.
    fn search_chain(&mut self, at: &Ctx<'_>, ip: usize, floor: usize) -> Match {
        self.insert_until(at, ip);
        let h = at.h;
        let cur = at.base + ip as u32;
        let lowest = at.base.max(cur.saturating_sub(at.max_dist as u32));
        let min_chain = cur.saturating_sub(self.chain_len());
        let max_len = at.end - ip;
        let mut best = Match::NONE;
        let mut best_len = floor;
        let mut v = self
            .hash
            .get(hash4(h, ip, self.search.hash_log))
            .copied()
            .unwrap_or(0);
        let mut tries = self.search.depth;
        while v >= lowest && v > 0 && tries > 0 {
            let c = (v - at.base) as usize;
            if h.get(c + best_len) == h.get(ip + best_len) {
                let len = common(h, c, ip, max_len);
                if len > best_len {
                    best_len = len;
                    best = Match {
                        len,
                        dist: ip - c,
                        rep: false,
                    };
                    if len >= self.search.target || len == max_len {
                        break;
                    }
                }
            }
            if v <= min_chain {
                break;
            }
            v = self
                .chain
                .get((v & self.chain_mask) as usize)
                .copied()
                .unwrap_or(0);
            tries -= 1;
        }
        best
    }
}

/// The buffer a block is searched in.
struct Ctx<'a> {
    h: &'a [u8],
    base: u32,
    end: usize,
    max_dist: usize,
}

impl Ctx<'_> {
    /// The match length at `ip` from `dist` back, 0 when that is out of reach.
    #[inline]
    fn len_at(&self, ip: usize, dist: usize) -> usize {
        if dist == 0 || dist > ip || dist > self.max_dist {
            return 0;
        }
        common(self.h, ip - dist, ip, self.end - ip)
    }
}

#[derive(Clone, Copy, Debug)]
struct Match {
    len: usize,
    dist: usize,
    rep: bool,
}

impl Match {
    const NONE: Self = Self {
        len: 0,
        dist: 0,
        rep: false,
    };

    /// What a match is worth: four per byte, less the bits its offset costs.
    fn gain(self) -> i64 {
        if self.len == 0 {
            return i64::MIN / 2;
        }
        let offset_bits = if self.rep {
            1
        } else {
            i64::from(31 - (self.dist as u32 + 3).leading_zeros())
        };
        4 * self.len as i64 - offset_bits
    }
}

/// Hash of the four bytes at `h[i..]`.
#[inline]
fn hash4(h: &[u8], i: usize, log: u32) -> usize {
    let word = h
        .get(i..i + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map_or(0, u32::from_le_bytes);
    (word.wrapping_mul(0x9E37_79B1) >> (32 - log)) as usize
}

/// How many bytes from `h[a..]` equal those from `h[b..]` (`a < b`), at most `max`.
#[inline]
pub(crate) fn common(h: &[u8], a: usize, b: usize, max: usize) -> usize {
    let (Some(x), Some(y)) = (h.get(a..a + max), h.get(b..b + max)) else {
        return 0;
    };
    let mut n = 0;
    let (xs, x_tail) = x.as_chunks::<8>();
    let (ys, y_tail) = y.as_chunks::<8>();
    for (p, q) in xs.iter().zip(ys) {
        let diff = u64::from_le_bytes(*p) ^ u64::from_le_bytes(*q);
        if diff != 0 {
            return n + (diff.trailing_zeros() / 8) as usize;
        }
        n += 8;
    }
    n + x_tail
        .iter()
        .zip(y_tail)
        .take_while(|(p, q)| p == q)
        .count()
}

#[cfg(test)]
mod tests {
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
}
