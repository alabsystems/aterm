// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::WarmBlock;
use crate::line::{Line, deserialize_page_lines, serialize_lines};
use crate::{ScrollbackError, decompress_lz4_bounded};

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
        let lines = self.logical_suffix(&decompressed, stored_line_count)?;
        if stored_line_count == self.line_count {
            // No trim occurred ⇒ `decompressed` == serialize_lines(lines) (it is what
            // `logical_suffix` just deserialized), so skip the re-serialize.
            Ok(decompressed)
        } else {
            Ok(serialize_lines(&lines))
        }
    }
}
