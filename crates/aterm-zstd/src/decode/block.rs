// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/decoding/block_decoder.rs,
// src/blocks/block.rs and src/decoding/scratch.rs, modified by the aterm
// project in 2026. See ../../LICENSE-MIT for the upstream MIT license.

//! Blocks (RFC 8878 §3.1.1.2): a 3-byte header, then raw bytes, one byte to
//! repeat, or a compressed block's literals and sequences sections.

use std::io::{self, Read};

use super::error::DecodeError;
use super::huffman::HuffmanTable;
use super::literals;
use super::sequences::{self, Exec, INITIAL_OFFSETS, Tables};
use super::window::{BlockBuf, SLACK, Window};

/// The per-frame decoding state that outlives one block: the entropy tables
/// a later block may repeat, the repeat offsets, and reusable buffers.
pub(crate) struct BlockDecoder {
    /// A compressed block's bytes.
    content: Vec<u8>,
    /// Literals decoded from Huffman streams or an RLE byte.
    literals: Vec<u8>,
    huffman: HuffmanTable,
    tables: Tables,
    offsets: [u32; 3],
}

impl BlockDecoder {
    pub(crate) fn new() -> Self {
        Self {
            content: Vec::new(),
            literals: Vec::new(),
            huffman: HuffmanTable::new(),
            tables: Tables::new(),
            offsets: INITIAL_OFFSETS,
        }
    }

    /// Forgets everything a previous frame left (frames are independent).
    pub(crate) fn reset(&mut self) {
        self.huffman.reset();
        self.tables.reset();
        self.offsets = INITIAL_OFFSETS;
    }

    /// Reads one block from `input` and decodes it into `out` (emptied
    /// first), never past `block_max` bytes. Returns whether it was the
    /// frame's last block.
    pub(crate) fn decode(
        &mut self,
        input: &mut impl Read,
        block_max: usize,
        history: &Window,
        out: &mut BlockBuf,
    ) -> io::Result<bool> {
        let mut header = [0u8; 3];
        input.read_exact(&mut header)?;
        let word = u32::from(header[0]) | u32::from(header[1]) << 8 | u32::from(header[2]) << 16;
        let last = word & 1 != 0;
        let size = (word >> 3) as usize;
        if size > block_max {
            return Err(DecodeError::Corrupt("block larger than the frame allows").into());
        }
        out.start(block_max);
        match (word >> 1) & 3 {
            // Raw_Block.
            0 => input.read_exact(out.fill_with(size)?)?,
            // RLE_Block: one byte, repeated `size` times.
            1 => {
                let mut byte = [0u8; 1];
                input.read_exact(&mut byte)?;
                out.fill_with(size)?.fill(byte[0]);
            }
            // Compressed_Block; zeroed slack past its bytes for chunked
            // copies out of raw literals.
            2 => {
                self.content.clear();
                self.content.resize(size + SLACK, 0);
                input.read_exact(self.content.get_mut(..size).unwrap_or_default())?;
                self.decompress(size, block_max, history, out)?;
            }
            _ => return Err(DecodeError::Corrupt("reserved block type").into()),
        }
        Ok(last)
    }

    /// A compressed block of `size` bytes: the literals section, then the
    /// sequences.
    fn decompress(
        &mut self,
        size: usize,
        block_max: usize,
        history: &Window,
        out: &mut BlockBuf,
    ) -> Result<(), DecodeError> {
        let Self {
            content,
            literals,
            huffman,
            tables,
            offsets,
        } = self;
        let (lits, lit_count, used) =
            literals::decode(content, size, huffman, literals, block_max)?;
        let rest = content.get(used..size).unwrap_or_default();
        let (header, header_len) = sequences::parse_header(rest)?;
        let rest = rest.get(header_len..).unwrap_or_default();
        if header.num_sequences == 0 {
            // The literals are the block.
            if !rest.is_empty() {
                return Err(DecodeError::Corrupt(
                    "bytes after a block with no sequences",
                ));
            }
            return out.push(lits.get(..lit_count).unwrap_or_default());
        }
        let mut exec = Exec {
            out,
            history,
            offsets,
        };
        sequences::execute(&header, rest, tables, lits, lit_count, &mut exec)
    }
}
