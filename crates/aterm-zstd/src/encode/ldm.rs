// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The long-distance matcher: long repeats anywhere in a window too wide for the
//! binary tree to hold, offered to the optimal parser beside the tree's matches.
//!
//! A rolling (gear) hash over the input picks anchors by content, about one position in
//! `1 << rate_log`: where the hash's top bits over the last `min_len` bytes are zero. The
//! same bytes give the same anchors wherever they sit, so a repeat of at least a few
//! dozen bytes shares anchors with its source. Each anchor's `min_len` bytes are hashed
//! into a table of small buckets; an anchor whose bytes some earlier anchor in its bucket
//! shares is a match, grown forward to the block's end and backward to where the last one
//! ended. This is libzstd's `--long` matcher in shape; which anchors are taken, and
//! whether a match is used, are coding choices (any match the format can express is one,
//! RFC 8878 §3.1.1.4).

use super::matcher::common;
use crate::xxh64::xxh64;

/// Matcher parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LdmParams {
    /// The table holds `1 << table_log` anchors.
    pub(crate) table_log: u32,
    /// Each bucket holds `1 << bucket_log` of them.
    pub(crate) bucket_log: u32,
    /// The bytes an anchor covers, and the shortest match reported (at most 64).
    pub(crate) min_len: u32,
    /// About one position in `1 << rate_log` is an anchor.
    pub(crate) rate_log: u32,
}

/// A long match found in a block: `h[start..end]` repeats `dist` bytes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LongMatch {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) dist: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    /// The anchor's virtual position (0: empty).
    pos: u32,
    /// The low bits of its bytes' hash, to skip most byte comparisons.
    check: u32,
}

/// The gear hash's per-byte values: a fixed pseudo-random table (SplitMix64).
const GEAR: [u64; 256] = {
    let mut t = [0u64; 256];
    let mut x: u64 = 0x5EED_1DE5_0F1C_0DE5;
    let mut i = 0;
    while i < 256 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        t[i] = z ^ (z >> 31);
        i += 1;
    }
    t
};

#[derive(Debug)]
pub(crate) struct Ldm {
    p: LdmParams,
    table: Vec<Entry>,
    /// Each bucket's next slot (its entries form a ring).
    cursor: Vec<u8>,
    /// The bits of the gear hash that pick an anchor: they depend on the last `min_len`
    /// bytes only.
    stop: u64,
}

impl Ldm {
    pub(crate) fn new(p: LdmParams) -> Self {
        let bits = p.min_len.clamp(p.rate_log.max(1), 64);
        let stop = ((1u64 << p.rate_log) - 1) << (bits - p.rate_log);
        Self {
            p,
            table: vec![Entry::default(); 1 << p.table_log],
            cursor: vec![0; 1 << (p.table_log - p.bucket_log)],
            stop,
        }
    }

    /// Moves every position down by `by`, forgetting those below `keep_from`.
    pub(crate) fn rebase(&mut self, by: u32, keep_from: u32) {
        for e in &mut self.table {
            e.pos = if e.pos >= keep_from { e.pos - by } else { 0 };
        }
    }

    /// The long matches of block `h[start..end]` into `out`, in order and apart; anchors
    /// in it join the table. `base` is the position of `h[0]`.
    pub(crate) fn find(
        &mut self,
        h: &[u8],
        base: u32,
        start: usize,
        end: usize,
        max_dist: usize,
        out: &mut Vec<LongMatch>,
    ) {
        out.clear();
        let min = self.p.min_len as usize;
        if end < start + min {
            return;
        }
        let bucket_shift = 64 - (self.p.table_log - self.p.bucket_log);
        let bucket_len = 1usize << self.p.bucket_log;
        // The hash at the block's first byte, from the bytes before it (the bits that pick
        // an anchor forget anything older).
        let mut hash = 0u64;
        for &b in h.get(start.saturating_sub(64)..start).unwrap_or_default() {
            hash = (hash << 1).wrapping_add(GEAR[usize::from(b)]);
        }
        // Where the last match ended: anchors before it only join the table.
        let mut matched_to = start;
        for i in start..end {
            let b = h.get(i).copied().unwrap_or(0);
            hash = (hash << 1).wrapping_add(GEAR[usize::from(b)]);
            if hash & self.stop != 0 || i + 1 < start + min {
                continue;
            }
            // The anchor covers `h[a..i + 1]`.
            let a = i + 1 - min;
            let Some(bytes) = h.get(a..i + 1) else {
                continue;
            };
            let key = xxh64(0, bytes);
            let bucket = (key >> bucket_shift) as usize * bucket_len;
            let check = key as u32;
            let pos = base + a as u32;
            let cursor = self
                .cursor
                .get_mut(bucket >> self.p.bucket_log)
                .map_or(0, |c| {
                    let at = usize::from(*c);
                    *c = c.wrapping_add(1) & (bucket_len - 1) as u8;
                    at
                });
            let Some(entries) = self.table.get_mut(bucket..bucket + bucket_len) else {
                continue;
            };
            if a >= matched_to {
                let low = base.max((pos).saturating_sub(max_dist as u32)).max(1);
                let mut best: Option<(usize, usize, u32)> = None;
                for e in entries.iter() {
                    if e.check != check || e.pos < low || e.pos >= pos {
                        continue;
                    }
                    let c = (e.pos - base) as usize;
                    let forward = common(h, c, a, end - a);
                    if forward < min {
                        continue;
                    }
                    let mut back = 0;
                    while a - back > matched_to
                        && c > back
                        && h.get(a - back - 1) == h.get(c - back - 1)
                    {
                        back += 1;
                    }
                    if best.is_none_or(|(f, k, _)| forward + back > f + k) {
                        best = Some((forward, back, pos - e.pos));
                    }
                }
                if let Some((forward, back, dist)) = best {
                    out.push(LongMatch {
                        start: a - back,
                        end: a + forward,
                        dist,
                    });
                    matched_to = a + forward;
                }
            }
            if let Some(e) = entries.get_mut(cursor) {
                *e = Entry { pos, check };
            }
        }
    }
}

#[cfg(test)]
#[path = "ldm_tests.rs"]
mod tests;
