// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Blocks (RFC 8878 §3.1.1.2): a block's bytes and the sequences a match finder found in
//! them become a raw, RLE or compressed block, whichever is smallest (or, cut at sequence
//! boundaries, several, where that costs less), and the entropy state a decoder carries
//! from block to block moves only with the blocks it decodes.

use super::huffman::Code;
use super::literals;
use super::sequences::{self, Coded, Held, Predefined, Seq};
use crate::decode::sequences::INITIAL_OFFSETS;

/// `Block_Type` values (RFC 8878 §3.1.1.2.2).
const RAW_BLOCK: u32 = 0;
const RLE_BLOCK: u32 = 1;
const COMPRESSED_BLOCK: u32 = 2;

/// What a decoder carries across a frame's blocks: the literals code, the three
/// sequence tables and the repeat offsets.
#[derive(Clone, Debug)]
struct Entropy {
    huffman: Option<Code>,
    tables: [Held; 3],
    reps: [u32; 3],
}

impl Entropy {
    const fn new() -> Self {
        Self {
            huffman: None,
            tables: [Held::None, Held::None, Held::None],
            reps: INITIAL_OFFSETS,
        }
    }
}

/// Encodes a frame's blocks one after another.
#[derive(Debug)]
pub(crate) struct BlockEncoder {
    entropy: Entropy,
    predefined: Predefined,
    lits: Vec<u8>,
    coded: Vec<Coded>,
    body: Vec<u8>,
    #[cfg(test)]
    pub(crate) rollback: Rollback,
}

/// Test seam on the rollback in [`BlockEncoder::compress`]: how many blocks had sequences
/// coded and were then written raw, and a switch that keeps such a block's entropy state
/// anyway (the fault the rollback prevents).
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Rollback {
    pub(crate) rejected: usize,
    pub(crate) keep_rejected: bool,
}

#[cfg(test)]
impl Rollback {
    fn rejected(&mut self, trial: Entropy, entropy: &mut Entropy, seqs: usize) {
        if seqs > 0 {
            self.rejected += 1;
            if self.keep_rejected {
                *entropy = trial;
            }
        }
    }
}

impl BlockEncoder {
    pub(crate) fn new() -> Self {
        Self {
            entropy: Entropy::new(),
            predefined: Predefined::new(),
            lits: Vec::new(),
            coded: Vec::new(),
            body: Vec::new(),
            #[cfg(test)]
            rollback: Rollback::default(),
        }
    }

    /// The repeat offsets a decoder holds before the next block.
    pub(crate) const fn reps(&self) -> [u32; 3] {
        self.entropy.reps
    }

    /// Appends block `src` (with the `Last_Block` bit `last`) to `out`. `seqs` are the
    /// matches found in it, in order, covering it with the literals between them; `None`
    /// when no search ran, which allows only a raw or RLE block.
    ///
    /// With `split`, the block may go out as several: where its halves, each with tables
    /// fit to its own sequences, cost less than the whole ([`Self::cuts`]). Every part is a
    /// block of its own (RFC 8878 §3.1.1.2), and the repeat offsets and the tables a later
    /// part reuses carry across them as across any blocks (§3.1.1.5, §3.1.1.3.2.1).
    ///
    /// A compressed block is written only when it is smaller than the raw one (RFC 8878
    /// §3.1.1.2.4); otherwise the decoder's entropy state is left as it was, since a raw or
    /// RLE block does not touch it.
    pub(crate) fn write(
        &mut self,
        src: &[u8],
        seqs: Option<&[Seq]>,
        split: bool,
        last: bool,
        out: &mut Vec<u8>,
    ) {
        let Some(seqs) = seqs.filter(|s| split && s.len() >= 2 * SPLIT_MIN) else {
            return self.write_part(src, seqs, last, out);
        };
        let starts = starts(seqs);
        let cuts = self.cuts(src, seqs, &starts);
        let mut from = 0;
        for &to in cuts.iter().chain([&seqs.len()]) {
            let part = span(src, &starts, from, to);
            let part_seqs = seqs.get(from..to).unwrap_or_default();
            self.write_part(part, Some(part_seqs), last && to == seqs.len(), out);
            from = to;
        }
    }

    /// One block, as [`Self::write`] without splitting.
    fn write_part(&mut self, src: &[u8], seqs: Option<&[Seq]>, last: bool, out: &mut Vec<u8>) {
        if let Some((&first, rest)) = src.split_first()
            && rest.iter().all(|&b| b == first)
            && src.len() > 1
        {
            out.extend_from_slice(&header(RLE_BLOCK, src.len(), last));
            out.push(first);
            return;
        }
        if let Some(seqs) = seqs {
            let from = self.entropy.clone();
            match self.compress(&from, src, seqs) {
                Ok(trial) => {
                    out.extend_from_slice(&header(COMPRESSED_BLOCK, self.body.len(), last));
                    out.extend_from_slice(&self.body);
                    self.entropy = trial;
                    return;
                }
                #[cfg(test)]
                Err(Some(trial)) => self
                    .rollback
                    .rejected(*trial, &mut self.entropy, seqs.len()),
                Err(_) => {}
            }
        }
        out.extend_from_slice(&header(RAW_BLOCK, src.len(), last));
        out.extend_from_slice(src);
    }

    /// Where a block's sequences are cut into parts, as the index of each later part's
    /// first sequence. A part is halved (at its middle sequence, down to [`SPLIT_MIN`] a
    /// half and [`SPLIT_DEPTH`] halvings) when its two halves cost less, written one after
    /// the other from the state before it, than it does whole; this is a coding choice,
    /// since any cut at a sequence boundary is a valid pair of blocks.
    fn cuts(&mut self, src: &[u8], seqs: &[Seq], starts: &[usize]) -> Vec<usize> {
        let mut cuts = Vec::new();
        let from = self.entropy.clone();
        let whole = self.cost(&from, span(src, starts, 0, seqs.len()), seqs);
        let mut parts = Parts {
            src,
            seqs,
            starts,
            cuts: &mut cuts,
        };
        self.halve(&mut parts, 0, seqs.len(), &from, whole, SPLIT_DEPTH);
        cuts
    }

    /// [`Self::cuts`] over `seqs[lo..hi]`, written after state `from` at cost `whole`;
    /// returns the state after it, however it is cut.
    fn halve(
        &mut self,
        p: &mut Parts<'_>,
        lo: usize,
        hi: usize,
        from: &Entropy,
        whole: (usize, Entropy),
        depth: u32,
    ) -> Entropy {
        if depth == 0 || hi - lo < 2 * SPLIT_MIN {
            return whole.1;
        }
        let mid = lo + (hi - lo) / 2;
        let left = self.cost(
            from,
            span(p.src, p.starts, lo, mid),
            p.seqs.get(lo..mid).unwrap_or_default(),
        );
        let right = self.cost(
            &left.1,
            span(p.src, p.starts, mid, hi),
            p.seqs.get(mid..hi).unwrap_or_default(),
        );
        if left.0 + right.0 >= whole.0 {
            return whole.1;
        }
        let after = self.halve(p, lo, mid, from, left, depth - 1);
        p.cuts.push(mid);
        let right = self.cost(
            &after,
            span(p.src, p.starts, mid, hi),
            p.seqs.get(mid..hi).unwrap_or_default(),
        );
        self.halve(p, mid, hi, &after, right, depth - 1)
    }

    /// What block `src` with `seqs` costs written after state `from`, its header counted:
    /// compressed when that is smaller, else raw; and the state after it.
    fn cost(&mut self, from: &Entropy, src: &[u8], seqs: &[Seq]) -> (usize, Entropy) {
        match self.compress(from, src, seqs) {
            Ok(trial) => (3 + self.body.len(), trial),
            Err(_) => (3 + src.len(), from.clone()),
        }
    }

    /// The compressed block of `src` in `self.body` and the entropy state after it,
    /// written after state `from`. `Err` when it is not smaller than `src`, with the state
    /// its coded sequences would have left (`None` when they were not coded).
    fn compress(
        &mut self,
        from: &Entropy,
        src: &[u8],
        seqs: &[Seq],
    ) -> Result<Entropy, Option<Box<Entropy>>> {
        self.lits.clear();
        let mut at = 0usize;
        for s in seqs {
            let end = at + s.lit_len as usize;
            self.lits.extend_from_slice(src.get(at..end).ok_or(None)?);
            at = end + s.match_len as usize;
        }
        self.lits.extend_from_slice(src.get(at..).ok_or(None)?);

        let mut trial = from.clone();
        self.body.clear();
        if let Some(code) = literals::write(&self.lits, trial.huffman.as_ref(), &mut self.body) {
            trial.huffman = Some(code);
        }
        if self.body.len() >= src.len() {
            return Err(None);
        }
        sequences::write(
            seqs,
            &mut trial.reps,
            &mut trial.tables,
            &self.predefined,
            &mut self.coded,
            &mut self.body,
        );
        if self.body.len() < src.len() {
            Ok(trial)
        } else {
            Err(Some(Box::new(trial)))
        }
    }
}

/// The fewest sequences a split-off part holds: fewer cannot pay for the tables a part
/// describes (libzstd's splitter holds the same floor).
const SPLIT_MIN: usize = 300;

/// How many times a block is halved, at most: up to eight parts.
const SPLIT_DEPTH: u32 = 3;

/// A block being cut: its bytes, its sequences, where each sequence starts, and the cuts
/// so far.
struct Parts<'a> {
    src: &'a [u8],
    seqs: &'a [Seq],
    starts: &'a [usize],
    cuts: &'a mut Vec<usize>,
}

/// Where each sequence's literals start in the block, and after the last, where its match
/// ends.
fn starts(seqs: &[Seq]) -> Vec<usize> {
    let mut at = 0;
    let mut starts = Vec::with_capacity(seqs.len() + 1);
    for s in seqs {
        starts.push(at);
        at += (s.lit_len + s.match_len) as usize;
    }
    starts.push(at);
    starts
}

/// The bytes of the part holding `seqs[from..to]`: from its first sequence's literals to
/// its last's match end, or to the block's end (its last literals) for the last part.
fn span<'a>(src: &'a [u8], starts: &[usize], from: usize, to: usize) -> &'a [u8] {
    let begin = starts.get(from).copied().unwrap_or(src.len());
    let end = if to + 1 >= starts.len() {
        src.len()
    } else {
        starts.get(to).copied().unwrap_or(src.len())
    };
    src.get(begin..end).unwrap_or_default()
}

/// A `Block_Header` (RFC 8878 §3.1.1.2): `Last_Block` in bit 0, `Block_Type` in bits 1-2,
/// `Block_Size` in bits 3-23, little-endian. An RLE block's size is its decoded size.
fn header(kind: u32, size: usize, last: bool) -> [u8; 3] {
    let word = (size as u32) << 3 | kind << 1 | u32::from(last);
    let [a, b, c, _] = word.to_le_bytes();
    [a, b, c]
}

#[cfg(test)]
mod tests {
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
}
