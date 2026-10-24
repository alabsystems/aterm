// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The optimal parser: a block's sequences chosen by price, not by match length.
//!
//! Every symbol a sequence costs (RFC 8878 §3.1.1.3.2: a literal's Huffman code, the
//! literal and match length codes and their extra bits, the offset code and its extra
//! bits) is priced from the statistics of what the frame coded so far, in 1/256 bit. A
//! forward pass over up to [`HORIZON`] positions finds, for each, the cheapest way to
//! reach it: a literal after the cheapest way to reach the position before, or a match
//! from any [`super::tree`] match at an earlier position; a backward pass then reads the
//! cheapest path off as sequences. Repeat offsets (§3.1.1.5) are carried along each path,
//! so a match at a repeat distance is priced as the short code it is.
//!
//! This is the shape of libzstd's `btopt` and `btultra` strategies; the coding choices
//! (the price model, the horizon, when a long match ends the pass) are not part of the
//! format, which any parse satisfies.

use super::ldm::{Ldm, LdmParams, LongMatch};
use super::sequences::{Seq, ll_code, ml_code};
use super::tree::{Cand, Ctx, Tree, TreeParams};
use crate::decode::sequences::{LL_CODES, ML_CODES, resolve_offset};
use crate::frame::BLOCK_MAX;

/// Prices are in `1 / (1 << ACCURACY)` bits.
const ACCURACY: u32 = 8;
const BIT: i32 = 1 << ACCURACY;
/// A price no path reaches.
const UNREACHED: i32 = 1 << 30;

/// The positions one forward pass looks ahead.
pub(crate) const HORIZON: usize = 1 << 12;

/// A literal counts this much more than a sequence's codes in the statistics, so the
/// literal prices follow the data more quickly.
const LIT_WEIGHT: u32 = 2;

/// Parser parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OptParams {
    pub(crate) tree: TreeParams,
    /// Fractional-bit prices, the second look at a literal after a match, and every
    /// match length priced (libzstd's `btultra`); otherwise whole-bit prices and the
    /// pruning that makes the pass faster (`btopt`).
    pub(crate) ultra: bool,
    /// The frame's first block is parsed twice, the first pass only to learn prices
    /// (libzstd's `btultra2`).
    pub(crate) two_pass: bool,
    /// Long matches past the tree's reach, from [`super::ldm`] (a window wider than the
    /// tree's ring).
    pub(crate) ldm: Option<LdmParams>,
}

/// The cheapest way found to reach a position: a match (`mlen` bytes at `off`, or none
/// at the start of the pass), then `litlen` literals.
#[derive(Clone, Copy, Debug, Default)]
struct Node {
    price: i32,
    off: u32,
    mlen: u32,
    litlen: u32,
    /// The repeat offsets after the match.
    rep: [u32; 3],
}

/// Symbol statistics, and the prices they give.
#[derive(Clone, Debug)]
struct Stats {
    lit: [u32; 256],
    ll: [u32; 36],
    ml: [u32; 53],
    of: [u32; 32],
    sums: [u32; 4],
    /// `weight` of each sum: a symbol's price is its sum's less its own count's.
    bases: [i32; 4],
    started: bool,
    frac: bool,
}

const LIT: usize = 0;
const LL: usize = 1;
const ML: usize = 2;
const OF: usize = 3;

/// About `log2(stat + 1)` in 1/256 bit, plus one bit: the fraction is linear between
/// powers of two when `frac`, else dropped.
#[inline]
const fn weight(stat: u32, frac: bool) -> i32 {
    let stat = stat + 1;
    let hb = 31 - stat.leading_zeros();
    let whole = (hb << ACCURACY) as i32;
    if frac {
        whole + ((stat << ACCURACY) >> hb) as i32
    } else {
        whole
    }
}

/// Halves (or more) every count when the table's sum passes `2^log`, keeping each at
/// least 1; the new sum.
fn rescale(table: &mut [u32], log: u32) -> u32 {
    let sum: u32 = table.iter().sum();
    let factor = sum >> log;
    if factor <= 1 {
        return sum;
    }
    let shift = 31 - factor.leading_zeros();
    table.iter_mut().fold(0, |s, c| {
        *c = 1 + (*c >> shift);
        s + *c
    })
}

impl Stats {
    const fn new(frac: bool) -> Self {
        Self {
            lit: [0; 256],
            ll: [0; 36],
            ml: [0; 53],
            of: [0; 32],
            sums: [0; 4],
            bases: [0; 4],
            started: false,
            frac,
        }
    }

    /// Before a block: the first block's literal statistics come from its own bytes and
    /// the codes' from a typical frame; after that the frame's are scaled down, so the
    /// last blocks weigh most.
    fn start_block(&mut self, src: &[u8]) {
        if self.started {
            self.sums[LIT] = rescale(&mut self.lit, 12);
            self.sums[LL] = rescale(&mut self.ll, 11);
            self.sums[ML] = rescale(&mut self.ml, 11);
            self.sums[OF] = rescale(&mut self.of, 11);
        } else {
            self.started = true;
            let mut counts = [0u32; 256];
            for &b in src {
                if let Some(c) = counts.get_mut(usize::from(b)) {
                    *c += 1;
                }
            }
            for (l, c) in self.lit.iter_mut().zip(counts) {
                *l = u32::from(c > 0) + (c >> 8);
            }
            self.ll = [0; 36];
            for (i, l) in self.ll.iter_mut().enumerate() {
                *l = match i {
                    0 => 4,
                    1 => 2,
                    _ => 1,
                };
            }
            self.ml = [1; 53];
            self.of = [
                6, 2, 1, 1, 2, 3, 4, 4, 4, 3, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1,
            ];
            self.sums = [
                self.lit.iter().sum(),
                self.ll.iter().sum(),
                self.ml.iter().sum(),
                self.of.iter().sum(),
            ];
        }
        self.set_bases();
    }

    fn set_bases(&mut self) {
        for (b, &s) in self.bases.iter_mut().zip(&self.sums) {
            *b = weight(s, self.frac);
        }
    }

    /// One literal's price.
    #[inline]
    fn lit(&self, b: u8) -> i32 {
        let base = self.bases[LIT];
        let w = weight(
            self.lit.get(usize::from(b)).copied().unwrap_or(0),
            self.frac,
        );
        base - w.min(base - BIT)
    }

    /// The price of a literal length's code and extra bits.
    #[inline]
    fn ll(&self, litlen: u32) -> i32 {
        // A whole block of literals has no code; it costs a bit more than one less.
        if litlen as usize >= BLOCK_MAX {
            return BIT + self.ll(BLOCK_MAX as u32 - 1);
        }
        let code = usize::from(ll_code(litlen));
        let extra = LL_CODES.get(code).map_or(0, |c| i32::from(c.1));
        extra * BIT + self.bases[LL] - weight(self.ll.get(code).copied().unwrap_or(0), self.frac)
    }

    /// What one more literal costs in its literal length.
    #[inline]
    fn ll_step(&self, litlen: u32) -> i32 {
        self.ll(litlen) - self.ll(litlen - 1)
    }

    /// The price of a match's offset and length codes and extra bits.
    #[inline]
    fn matched(&self, off: u32, mlen: u32) -> i32 {
        let of_code = 31 - off.leading_zeros();
        let mut price = (of_code << ACCURACY) as i32 + self.bases[OF]
            - weight(
                self.of.get(of_code as usize).copied().unwrap_or(0),
                self.frac,
            );
        if !self.frac && of_code >= 20 {
            // Far offsets cost a decoder cache misses (libzstd's `btopt` handicap).
            price += (of_code as i32 - 19) * 2 * BIT;
        }
        let code = usize::from(ml_code(mlen));
        let extra = ML_CODES.get(code).map_or(0, |c| i32::from(c.1));
        price += extra * BIT + self.bases[ML]
            - weight(self.ml.get(code).copied().unwrap_or(0), self.frac);
        // A sequence fewer is worth a fifth of a bit: fewer sequences decode faster.
        price + BIT / 5
    }

    /// Counts a chosen sequence.
    fn add(&mut self, lits: &[u8], off: u32, mlen: u32) {
        for &b in lits {
            if let Some(c) = self.lit.get_mut(usize::from(b)) {
                *c += LIT_WEIGHT;
            }
        }
        self.sums[LIT] += lits.len() as u32 * LIT_WEIGHT;
        if let Some(c) = self.ll.get_mut(usize::from(ll_code(lits.len() as u32))) {
            *c += 1;
        }
        if let Some(c) = self.of.get_mut((31 - off.leading_zeros()) as usize) {
            *c += 1;
        }
        if let Some(c) = self.ml.get_mut(usize::from(ml_code(mlen))) {
            *c += 1;
        }
        self.sums[LL] += 1;
        self.sums[OF] += 1;
        self.sums[ML] += 1;
    }
}

/// The repeat offsets after a match at `off` (an `Offset_Value`) after `litlen`
/// literals: a decoder's rule (RFC 8878 §3.1.1.5).
#[inline]
fn after(rep: [u32; 3], off: u32, ll0: bool) -> [u32; 3] {
    let mut rep = rep;
    resolve_offset(off, usize::from(!ll0), &mut rep);
    rep
}

#[derive(Debug)]
pub(crate) struct Parser {
    params: OptParams,
    tree: Tree,
    stats: Stats,
    opt: Vec<Node>,
    cands: Vec<Cand>,
    /// The path read back, last stretch first.
    path: Vec<Node>,
    /// The first block is still to come.
    first: bool,
    ldm: Option<Ldm>,
    /// The block's long matches.
    long: Vec<LongMatch>,
}

/// Offers the long match covering `p`, if any, after the tree's matches there when it is
/// longer than all of them.
#[inline]
fn add_long(long: &[LongMatch], p: usize, min: u32, cands: &mut Vec<Cand>) {
    let i = long.partition_point(|m| m.end <= p);
    if let Some(m) = long.get(i)
        && m.start <= p
    {
        let len = (m.end - p) as u32;
        if len >= min && cands.last().is_none_or(|c| len > c.len) {
            cands.push(Cand {
                len,
                off: m.dist + 3,
            });
        }
    }
}

impl Parser {
    /// A parser whose first position is `start`.
    pub(crate) fn new(params: OptParams, start: u32) -> Self {
        Self {
            params,
            tree: Tree::new(params.tree, start),
            stats: Stats::new(params.ultra),
            opt: vec![Node::default(); HORIZON + 3],
            cands: Vec::new(),
            path: Vec::new(),
            first: true,
            ldm: params.ldm.map(Ldm::new),
            long: Vec::new(),
        }
    }

    /// The ring's length: a rebase must move positions by a multiple of it.
    pub(crate) const fn ring_len(&self) -> u32 {
        self.tree.ring_len()
    }

    /// Moves every position down by `by`, forgetting those below `keep_from`.
    pub(crate) fn rebase(&mut self, by: u32, keep_from: u32) {
        self.tree.rebase(by, keep_from);
        if let Some(l) = self.ldm.as_mut() {
            l.rebase(by, keep_from);
        }
    }

    /// The sequences of block `h[start..end]` into `seqs`, as [`super::matcher::Matcher::find`]
    /// gives them; `reps` are the repeat offsets the decoder holds at the block's start.
    #[expect(
        clippy::too_many_arguments,
        reason = "the block, its history and the decoder state it is parsed against"
    )]
    pub(crate) fn find(
        &mut self,
        h: &[u8],
        base: u32,
        start: usize,
        end: usize,
        max_dist: usize,
        reps: [u32; 3],
        seqs: &mut Vec<Seq>,
    ) {
        let src = h.get(start..end).unwrap_or_default();
        match self.ldm.as_mut() {
            Some(l) => l.find(h, base, start, end, max_dist, &mut self.long),
            None => self.long.clear(),
        }
        if self.first && self.params.two_pass && src.len() > 8 {
            // Learn prices from a first parse of the block, then forget its positions.
            self.stats.start_block(src);
            let tree = std::mem::replace(
                &mut self.tree,
                Tree::new(self.params.tree, base + start as u32),
            );
            self.parse(h, base, start, end, max_dist, reps, seqs);
            self.tree = tree;
        }
        self.first = false;
        self.stats.start_block(src);
        self.parse(h, base, start, end, max_dist, reps, seqs);
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the block, its history and the decoder state it is parsed against"
    )]
    fn parse(
        &mut self,
        h: &[u8],
        base: u32,
        start: usize,
        end: usize,
        max_dist: usize,
        reps: [u32; 3],
        seqs: &mut Vec<Seq>,
    ) {
        seqs.clear();
        let at = Ctx {
            h,
            base,
            end,
            max_dist,
        };
        let ultra = self.params.ultra;
        let min = self.params.tree.min_match;
        let sufficient = self.params.tree.target.min(HORIZON as u32 - 1);
        // The last match starts at least 8 bytes before the block's end.
        let ilimit = end.saturating_sub(8);
        let mut rep = reps;
        let mut ip = start;
        let mut anchor = start;
        let byte = |i: usize| h.get(i).copied().unwrap_or(0);

        while ip < ilimit {
            let litlen = (ip - anchor) as u32;
            self.tree
                .matches(&at, ip, &rep, litlen == 0, &mut self.cands);
            add_long(&self.long, ip, min, &mut self.cands);
            let Some(&longest) = self.cands.last() else {
                ip += 1;
                continue;
            };
            let opt = &mut self.opt;
            let stats = &self.stats;
            let start_price = stats.ll(litlen);
            set(
                opt,
                0,
                Node {
                    price: start_price,
                    off: 0,
                    mlen: 0,
                    litlen,
                    rep,
                },
            );

            // `cur` is where the last stretch's match starts; `last_pos` the pass's end.
            let (mut cur, mut last_pos, last);
            if longest.len > sufficient {
                // A long match is taken as it is.
                cur = 0;
                last_pos = longest.len as usize;
                last = Node {
                    price: 0,
                    off: longest.off,
                    mlen: longest.len,
                    litlen: 0,
                    rep,
                };
            } else {
                for pos in 1..min as usize {
                    set(
                        opt,
                        pos,
                        Node {
                            price: UNREACHED,
                            litlen: litlen + pos as u32,
                            ..Node::default()
                        },
                    );
                }
                let mut pos = min as usize;
                for c in &self.cands {
                    while pos <= c.len as usize {
                        let price = start_price + stats.matched(c.off, pos as u32) + stats.ll(0);
                        set(
                            opt,
                            pos,
                            Node {
                                price,
                                off: c.off,
                                mlen: pos as u32,
                                litlen: 0,
                                rep,
                            },
                        );
                        pos += 1;
                    }
                }
                last_pos = pos - 1;
                unreach(opt, pos);

                let mut ended: Option<(usize, usize, Node)> = None;
                cur = 1;
                while cur <= last_pos {
                    let inr = ip + cur;
                    let prev = get(opt, cur - 1);
                    let litlen = prev.litlen + 1;
                    let price = prev.price + stats.lit(byte(inr - 1)) + stats.ll_step(litlen);
                    let here = get(opt, cur);
                    if price <= here.price {
                        set(
                            opt,
                            cur,
                            Node {
                                price,
                                litlen,
                                ..prev
                            },
                        );
                        // A match replaced by a literal: it may still pay as the match
                        // and one literal after it.
                        if ultra && here.litlen == 0 && stats.ll_step(1) < 0 && inr < end {
                            let lit = stats.lit(byte(inr));
                            let with_one = here.price + lit + stats.ll_step(1);
                            let with_more = price + lit + stats.ll_step(litlen + 1);
                            if with_one < with_more && with_one < get(opt, cur + 1).price {
                                let from = get(opt, cur - here.mlen as usize);
                                set(
                                    opt,
                                    cur + 1,
                                    Node {
                                        price: with_one,
                                        litlen: 1,
                                        rep: after(from.rep, here.off, from.litlen == 0),
                                        ..here
                                    },
                                );
                                if last_pos < cur + 1 {
                                    last_pos = cur + 1;
                                    unreach(opt, last_pos + 1);
                                }
                            }
                        }
                    }
                    // A match ends here: its repeat offsets are now known.
                    let node = get(opt, cur);
                    if node.litlen == 0 {
                        let from = get(opt, cur - node.mlen as usize);
                        let mut n = node;
                        n.rep = after(from.rep, node.off, from.litlen == 0);
                        set(opt, cur, n);
                    }
                    if inr > ilimit {
                        cur += 1;
                        continue;
                    }
                    if cur == last_pos {
                        break;
                    }
                    let node = get(opt, cur);
                    if !ultra && get(opt, cur + 1).price <= node.price + BIT / 2 {
                        // The next position is already as cheap: not worth a search.
                        cur += 1;
                        continue;
                    }
                    self.tree
                        .matches(&at, inr, &node.rep, node.litlen == 0, &mut self.cands);
                    add_long(&self.long, inr, min, &mut self.cands);
                    let Some(&longest) = self.cands.last() else {
                        cur += 1;
                        continue;
                    };
                    if longest.len > sufficient
                        || cur + longest.len as usize >= HORIZON
                        || inr + longest.len as usize >= end
                    {
                        ended = Some((
                            cur,
                            cur + longest.len as usize,
                            Node {
                                price: 0,
                                off: longest.off,
                                mlen: longest.len,
                                litlen: 0,
                                rep: node.rep,
                            },
                        ));
                        break;
                    }
                    let base_price = node.price + stats.ll(0);
                    let mut from_len = min;
                    for c in &self.cands {
                        let mut mlen = c.len;
                        while mlen >= from_len {
                            let pos = cur + mlen as usize;
                            let price = base_price + stats.matched(c.off, mlen);
                            if pos > last_pos || price < get(opt, pos).price {
                                while last_pos < pos {
                                    last_pos += 1;
                                    set(
                                        opt,
                                        last_pos,
                                        Node {
                                            price: UNREACHED,
                                            litlen: 1,
                                            ..Node::default()
                                        },
                                    );
                                }
                                set(
                                    opt,
                                    pos,
                                    Node {
                                        price,
                                        off: c.off,
                                        mlen,
                                        litlen: 0,
                                        rep: node.rep,
                                    },
                                );
                            } else if !ultra {
                                break;
                            }
                            mlen -= 1;
                        }
                        from_len = c.len + 1;
                    }
                    unreach(opt, last_pos + 1);
                    cur += 1;
                }
                match ended {
                    Some((c, l, node)) => {
                        cur = c;
                        last_pos = l;
                        last = node;
                    }
                    None => {
                        last = get(opt, last_pos);
                        // Where the last match starts (none when it is literals all along).
                        cur = if last.mlen == 0 {
                            0
                        } else {
                            last_pos - last.mlen as usize - last.litlen as usize
                        };
                    }
                }
            }

            if last.mlen == 0 {
                // Literals all the way: the pass found nothing worth its price.
                ip += last_pos;
                continue;
            }

            // Back from the last stretch to the pass's start: each stretch's match, and
            // the literals before it (the stretch before it's).
            self.path.clear();
            self.path.push(last);
            let mut pos = cur;
            loop {
                let s = get(&self.opt, pos);
                if s.mlen == 0 {
                    break;
                }
                self.path.push(s);
                pos -= (s.litlen + s.mlen) as usize;
            }
            let mut lits = get(&self.opt, pos).litlen;
            for i in (0..self.path.len()).rev() {
                let Some(&s) = self.path.get(i) else { break };
                let lit_start = anchor;
                anchor += lits as usize;
                let dist = resolve_offset(s.off, lits as usize, &mut rep);
                seqs.push(Seq {
                    lit_len: lits,
                    match_len: s.mlen,
                    offset: dist,
                });
                self.stats
                    .add(h.get(lit_start..anchor).unwrap_or_default(), s.off, s.mlen);
                anchor += s.mlen as usize;
                lits = s.litlen;
            }
            // The last stretch's literals start the next pass.
            ip = anchor + lits as usize;
            self.stats.set_bases();
        }
    }
}

#[inline]
fn get(opt: &[Node], i: usize) -> Node {
    opt.get(i).copied().unwrap_or(Node {
        price: UNREACHED,
        ..Node::default()
    })
}

#[inline]
fn set(opt: &mut [Node], i: usize, n: Node) {
    if let Some(slot) = opt.get_mut(i) {
        *slot = n;
    }
}

#[inline]
fn unreach(opt: &mut [Node], i: usize) {
    if let Some(slot) = opt.get_mut(i) {
        slot.price = UNREACHED;
    }
}

#[cfg(test)]
mod tests {
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
}
