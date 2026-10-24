// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Blocks (RFC 8878 §3.1.1.2): a block's bytes and the sequences a match finder found in
//! them become a raw, RLE or compressed block, whichever is smallest, and the entropy
//! state a decoder carries from block to block moves only with the blocks it decodes.

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
/// anyway (the fault the rollback prevents), so a test can show its input would see it.
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

    /// Appends block `src` (with the `Last_Block` bit `last`) to `out`. `seqs` are the
    /// matches found in it, in order, covering it with the literals between them; `None`
    /// when no search ran, which allows only a raw or RLE block.
    ///
    /// A compressed block is written only when it is smaller than the raw one (RFC 8878
    /// §3.1.1.2.4); otherwise the decoder's entropy state is left as it was, since a raw or
    /// RLE block does not touch it.
    pub(crate) fn write(
        &mut self,
        src: &[u8],
        seqs: Option<&[Seq]>,
        last: bool,
        out: &mut Vec<u8>,
    ) {
        if let Some((&first, rest)) = src.split_first()
            && rest.iter().all(|&b| b == first)
            && src.len() > 1
        {
            out.extend_from_slice(&header(RLE_BLOCK, src.len(), last));
            out.push(first);
            return;
        }
        if let Some(seqs) = seqs
            && let Some(trial) = self.compress(src, seqs)
        {
            out.extend_from_slice(&header(COMPRESSED_BLOCK, self.body.len(), last));
            out.extend_from_slice(&self.body);
            self.entropy = trial;
            return;
        }
        out.extend_from_slice(&header(RAW_BLOCK, src.len(), last));
        out.extend_from_slice(src);
    }

    /// The compressed block of `src` in `self.body` and the entropy state after it, or
    /// `None` when it is not smaller than `src`.
    fn compress(&mut self, src: &[u8], seqs: &[Seq]) -> Option<Entropy> {
        self.lits.clear();
        let mut at = 0usize;
        for s in seqs {
            let end = at + s.lit_len as usize;
            self.lits.extend_from_slice(src.get(at..end)?);
            at = end + s.match_len as usize;
        }
        self.lits.extend_from_slice(src.get(at..)?);

        let mut trial = self.entropy.clone();
        self.body.clear();
        if let Some(code) = literals::write(&self.lits, trial.huffman.as_ref(), &mut self.body) {
            trial.huffman = Some(code);
        }
        if self.body.len() >= src.len() {
            return None;
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
            return Some(trial);
        }
        #[cfg(test)]
        self.rollback.rejected(trial, &mut self.entropy, seqs.len());
        None
    }
}

/// A `Block_Header` (RFC 8878 §3.1.1.2): `Last_Block` in bit 0, `Block_Type` in bits 1-2,
/// `Block_Size` in bits 3-23, little-endian. An RLE block's size is its decoded size.
fn header(kind: u32, size: usize, last: bool) -> [u8; 3] {
    let word = (size as u32) << 3 | kind << 1 | u32::from(last);
    let [a, b, c, _] = word.to_le_bytes();
    [a, b, c]
}
