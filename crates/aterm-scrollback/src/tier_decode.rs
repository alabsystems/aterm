// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{WarmBlock, WarmTier};
use crate::line::{
    Line, MAX_DECODE_PAGE_LINES, count_page_lines, deserialize_page_lines, serialize_lines,
};
use crate::{ScrollbackError, decompress_lz4_bounded};

impl WarmTier {
    /// Resolve a PHYSICAL line index to `(block_idx, line_in_block)`.
    ///
    /// The single home for the cumulative-index geometry, shared by the
    /// random-access read path (`get_line`) and the block-streaming bulk
    /// path (`take_lines_from`/`segment_len_at`) so the two cannot drift.
    // Skip: same guarded-index class as `get_line`, which this was factored
    // from. total `get` + saturating: `find_block` returned an in-range
    // index and `block_start <= physical_idx` by construction; the verifier
    // cannot chain either fact. The unreachable arms yield the same values.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn locate(&self, physical_idx: usize) -> Option<(usize, usize)> {
        let block_idx = self.find_block(physical_idx)?;
        let block_start = if block_idx == 0 {
            0
        } else {
            self.cumulative_lines
                .get(block_idx.saturating_sub(1))
                .copied()
                .unwrap_or(0)
        };
        Some((block_idx, physical_idx.saturating_sub(block_start)))
    }

    /// Decode the block containing logical line `idx` and return OWNED lines
    /// from `idx` through the end of that block — the bulk-walk primitive
    /// (ST-6). One decode + one `split_off` per block: no per-line binary
    /// search, no per-line `Line` clone, and NO touch of the render-path
    /// block cache. Quarantined blocks error without a doomed decode,
    /// exactly like `get_line`.
    ///
    /// Returns an empty vec for an out-of-bounds `idx`.
    // Skip: the block lookup + decode path — same class as `get_line`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn take_lines_from(&self, idx: usize) -> Result<Vec<Line>, ScrollbackError> {
        if idx >= self.line_count {
            return Ok(Vec::new());
        }
        // Saturating: bounded by the tier's line totals — exact on every
        // real path (see get_line).
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
        // `min` keeps split_off total; a short decode yields a short
        // (possibly empty) segment, which the streaming iterator treats as
        // end-of-data — fail-closed, never a panic.
        let split_at = line_in_block.min(lines.len());
        Ok(lines.split_off(split_at))
    }

    /// Logical lines from `idx` through the end of its containing block —
    /// how far a bulk walk skips when that block fails to decode (its whole
    /// remaining span, matching the per-line skip total of the old
    /// line-at-a-time walk). Zero when out of bounds. Never decodes.
    // Skip: same guarded-index class as `locate`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn segment_len_at(&self, idx: usize) -> usize {
        if idx >= self.line_count {
            return 0;
        }
        let physical_idx = idx.saturating_add(self.front_offset);
        let Some((block_idx, _)) = self.locate(physical_idx) else {
            return 0;
        };
        let block_end = self.cumulative_lines.get(block_idx).copied().unwrap_or(0);
        // Physical block end -> logical, clamped by the tier's logical count
        // (the last block's physical end IS the logical end), minus `idx`.
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
        // Constant indexing under the dominating `len < 4` guard above:
        // identical bytes to the previous `serialized[..4].try_into().expect(..)`
        // (whose expect the strict gate cannot prove away and whose `try_into`
        // it cannot lower).
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
            // After a corrupt front-offset materialization we preserve the
            // surviving logical suffix by shrinking line_count. If the block
            // later decodes again, drop the consumed prefix lazily here.
            // lines.len() == stored_line_count (checked above) and
            // trimmed = stored_line_count - self.line_count <= stored_line_count,
            // so the `.min(lines.len())` clamp never engages and the call is
            // behavior-identical to `split_off(trimmed)`; it discharges the
            // split_off out-of-bounds panic obligation.
            let split_at = trimmed.min(lines.len());
            lines = lines.split_off(split_at);
        }
        self.decompress_failures.set(0);
        Ok(lines)
    }

    /// Decompress and get all lines.
    ///
    /// Increments the failure counter on any error so that read-path callers
    /// (`get_line`, iterators, `to_cold_compressed`) advance a corrupt block
    /// toward quarantine. The success path resets the counter in
    /// `logical_suffix`.
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

    /// Decompress to the SERIALIZED page bytes ready for cold re-compression,
    /// avoiding the redundant re-serialize `to_cold_compressed` used to do
    /// (`serialize_lines(&self.decompress()?)`) on every warm→cold eviction. Runs the
    /// SAME framing validation + failure bookkeeping as [`Self::decompress`] (the
    /// counter increments on any error; `logical_suffix` resets it + trims on
    /// success), then: with NO front-offset trim (`stored_line_count ==
    /// self.line_count`, the normal path) the LZ4 `decompressed` bytes ALREADY are the
    /// serialized form of the logical lines — they just round-tripped through
    /// `logical_suffix`'s deserialize — so reuse them; only a trimmed suffix needs a
    /// fresh `serialize_lines`. Correctness-preserving: the reused bytes are a valid
    /// serialization of the same lines (the cold tier deserializes them identically).
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
            // No trim ⇒ `decompressed` ALREADY is the serialization of this
            // block's logical lines, so the only thing wanted from them is the
            // framing VALIDATION `logical_suffix` performs — never the
            // `Vec<Line>` it materialized to get there and this arm then
            // dropped. Counting complete records instead removes one full
            // `Line` construct+destruct (heap content copy, boxed attrs `Rle`,
            // boxed hyperlink `SmallVec`) per line, and at the default tier
            // limits an eviction fires once per 100 pushed lines, so that
            // amortized to a wasted malloc/free chain for EVERY line aging out
            // of the warm tier — on the output thread.
            //
            // Byte-identical outcome: `count_page_lines` runs the SAME framing
            // walk with the SAME acceptance predicate (`walk_records` +
            // `Line::record_is_valid`, pinned to `Line::deserialize` by a
            // debug_assert in the decoder), under the same page cap; the
            // `stored_line_count < self.line_count` rejection is unreachable on
            // this branch; and the error text and the `decompress_failures`
            // reset are reproduced exactly.
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
        // Trimmed suffix: this is the one arm that genuinely consumes the lines.
        let lines = self.logical_suffix(&decompressed, stored_line_count)?;
        Ok(serialize_lines(&lines))
    }
}
