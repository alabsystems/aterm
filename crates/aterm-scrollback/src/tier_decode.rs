// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{WarmBlock, WarmTier};
use crate::line::{
    Line, MAX_DECODE_PAGE_LINES, count_page_lines, deserialize_page_lines, serialize_lines,
};
use crate::{ScrollbackError, decompress_lz4_bounded};

impl WarmTier {
    /// Resolve a physical line index to `(block_idx, line_in_block)`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn locate(&self, physical_idx: usize) -> Option<(usize, usize)> {
        let block_idx = self.find_block(physical_idx)?;
        let block_start = if block_idx == 0 {
            self.cumulative_base
        } else {
            self.live_cumulative()
                .get(block_idx.saturating_sub(1))
                .copied()
                .unwrap_or(self.cumulative_base)
        };
        Some((
            block_idx,
            physical_idx
                .saturating_add(self.cumulative_base)
                .saturating_sub(block_start),
        ))
    }

    /// Decode the block containing logical line `idx` and return owned lines from `idx` to the end
    /// of that block: the bulk-walk primitive.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn take_lines_from(&self, idx: usize) -> Result<Vec<Line>, ScrollbackError> {
        if idx >= self.line_count {
            return Ok(Vec::new());
        }
        let physical_idx = idx.saturating_add(self.front_offset);
        let Some((block_idx, line_in_block)) = self.locate(physical_idx) else {
            return Err(ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "in-range line index {idx} (physical {physical_idx}) has no backing warm block"
                ),
            )));
        };
        let Some(block) = self.blocks.get(block_idx) else {
            return Err(ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("warm block index {block_idx} out of range"),
            )));
        };
        if block.is_quarantined() {
            return Err(ScrollbackError::Quarantined(block.line_count()));
        }
        let mut lines = block.decompress()?;
        if line_in_block == 0 {
            return Ok(lines);
        }
        let split_at = line_in_block.min(lines.len());
        Ok(lines.split_off(split_at))
    }

    /// Logical lines from `idx` to the end of its containing block.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn segment_len_at(&self, idx: usize) -> usize {
        if idx >= self.line_count {
            return 0;
        }
        let physical_idx = idx.saturating_add(self.front_offset);
        let Some((block_idx, _)) = self.locate(physical_idx) else {
            return 0;
        };
        let block_end = self
            .live_cumulative()
            .get(block_idx)
            .copied()
            .unwrap_or(self.cumulative_base)
            .saturating_sub(self.cumulative_base);
        block_end
            .saturating_sub(self.front_offset)
            .min(self.line_count)
            .saturating_sub(idx)
    }
}

impl WarmBlock {
    fn stored_line_count(serialized: &[u8]) -> Result<usize, ScrollbackError> {
        if serialized.len() < 4 {
            return Err(ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "warm block serialized payload too short for count header",
            )));
        }
        Ok(
            u32::from_le_bytes([serialized[0], serialized[1], serialized[2], serialized[3]])
                as usize,
        )
    }

    fn logical_suffix(
        &self,
        serialized: &[u8],
        stored_line_count: usize,
    ) -> Result<Vec<Line>, ScrollbackError> {
        if stored_line_count < self.line_count {
            return Err(ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "warm block serialized {} lines but metadata claims {}",
                    stored_line_count, self.line_count
                ),
            )));
        }

        let mut lines = deserialize_page_lines(serialized);
        if lines.len() != stored_line_count {
            return Err(ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "warm block serialized {} lines but decoded {} complete lines",
                    stored_line_count,
                    lines.len()
                ),
            )));
        }

        let trimmed = stored_line_count.saturating_sub(self.line_count);
        if trimmed > 0 {
            let split_at = trimmed.min(lines.len());
            lines = lines.split_off(split_at);
        }
        self.decompress_failures.set(0);
        Ok(lines)
    }

    /// Decompress and get all lines.
    pub(crate) fn decompress(&self) -> Result<Vec<Line>, ScrollbackError> {
        let result = self.try_decompress();
        if result.is_err() {
            let failures = self.decompress_failures.get().saturating_add(1);
            self.decompress_failures.set(failures);
        }
        result
    }

    fn try_decompress(&self) -> Result<Vec<Line>, ScrollbackError> {
        let decompressed = decompress_lz4_bounded(&self.compressed)?;
        let stored_line_count = Self::stored_line_count(&decompressed)?;
        self.logical_suffix(&decompressed, stored_line_count)
    }

    /// Decompress to the serialized page bytes for cold re-compression, with the same framing
    /// validation and failure bookkeeping as [`Self::decompress`].
    pub(crate) fn decompress_serialized_page(&self) -> Result<Vec<u8>, ScrollbackError> {
        let result = self.try_decompress_serialized();
        if result.is_err() {
            let failures = self.decompress_failures.get().saturating_add(1);
            self.decompress_failures.set(failures);
        }
        result
    }

    fn try_decompress_serialized(&self) -> Result<Vec<u8>, ScrollbackError> {
        let decompressed = decompress_lz4_bounded(&self.compressed)?;
        let stored_line_count = Self::stored_line_count(&decompressed)?;
        if stored_line_count == self.line_count {
            let counted = count_page_lines(&decompressed, MAX_DECODE_PAGE_LINES);
            if counted != stored_line_count {
                return Err(ScrollbackError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "warm block serialized {} lines but decoded {} complete lines",
                        stored_line_count, counted
                    ),
                )));
            }
            self.decompress_failures.set(0);
            return Ok(decompressed);
        }
        let lines = self.logical_suffix(&decompressed, stored_line_count)?;
        Ok(serialize_lines(&lines))
    }
}
