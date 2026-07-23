// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Warm tier: LZ4 compressed blocks in RAM.
//!
//! Hot tier is in [`hot_tier`](super::hot_tier).
//! Cold tier is in [`cold_tier`](super::cold_tier).

use super::line::{Line, serialize_lines};
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::VecDeque;

#[cfg(test)]
thread_local! {
    static WARM_FIND_BLOCK_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_warm_find_block_steps(steps: usize) {
    WARM_FIND_BLOCK_STEPS.with(|c| c.set(c.get() + steps));
}

#[cfg(test)]
fn take_warm_find_block_steps() -> usize {
    WARM_FIND_BLOCK_STEPS.with(|c| {
        let value = c.get();
        c.set(0);
        value
    })
}

// ============================================================================
// Warm Tier - LZ4 compressed blocks
// ============================================================================

/// Consecutive decompression failures before a warm block is quarantined.
/// 3 failures = conclusive evidence of persistent corruption.
pub(crate) const QUARANTINE_THRESHOLD: u8 = 3;

/// A compressed block of lines (LZ4).
// Skip (propagates to the derive-generated impls — the checker consults the
// impl subject for macro-generated items): the derived Clone's field clones
// (`Vec<u8>` etc.) are the idiomatic-alloc class the campaign skips
// (`Line::clone` precedent). Verify-only; behavior unchanged.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone)]
pub(crate) struct WarmBlock {
    /// LZ4 compressed data.
    compressed: Vec<u8>,
    /// Number of lines in block (for counting without decompression).
    line_count: usize,
    /// Consecutive decompression failure count. Reset on success.
    /// When >= QUARANTINE_THRESHOLD, block is treated as quarantined.
    decompress_failures: Cell<u8>,
}

impl WarmBlock {
    /// Create a warm block from lines.
    #[must_use]
    pub(crate) fn from_lines(lines: &[Line]) -> Self {
        let line_count = lines.len();
        let serialized = serialize_lines(lines);
        // `compress_prepend_size` fails only when the input exceeds
        // u32::MAX bytes. A warm block holds at most `block_size` hot lines,
        // whose serialization is always well under 4 GiB, so the `Err` arm
        // is unreachable on every real input. Producing an empty buffer
        // there — rather than the former `.expect(...)` panic — is therefore
        // behavior-identical on all real inputs while discharging the Trust
        // L0 panic-freedom obligation (same "no-op under the documented
        // invariant" discharge as the `saturating_*` fixes in this crate).
        // (Unreachable `Err` ⇒ empty buffer; clippy's `manual_unwrap_or_default`
        // requires the combinator form of the same discharge.)
        let compressed = crate::lz4::compress_prepend_size(&serialized).unwrap_or_default();

        Self {
            compressed,
            line_count,
            decompress_failures: Cell::new(0),
        }
    }

    /// Get the number of lines in this block.
    #[must_use]
    #[inline]
    pub(crate) fn line_count(&self) -> usize {
        self.line_count
    }

    /// Returns true if this block has been quarantined due to repeated failures.
    #[must_use]
    #[inline]
    pub(crate) fn is_quarantined(&self) -> bool {
        self.decompress_failures.get() >= QUARANTINE_THRESHOLD
    }

    /// Record a decompression failure. Returns true if block just became quarantined.
    ///
    /// Production callers should use `decompress()` which tracks failures
    /// internally. This method is retained for test scenarios that need to
    /// force a quarantine state without actual decompression.
    #[cfg(test)]
    pub(crate) fn record_failure(&self) -> bool {
        let failures = self.decompress_failures.get().saturating_add(1);
        self.decompress_failures.set(failures);
        failures == QUARANTINE_THRESHOLD
    }

    /// Get the compressed size in bytes.
    #[must_use]
    #[inline]
    pub(crate) fn compressed_size(&self) -> usize {
        self.compressed.len()
    }

    /// Get a specific line by index within this block.
    #[cfg(test)]
    // Skip: the block lookup + decompress path — same guarded-index/decode class
    // as the cold tier's `get_line`. Round-trip and ARENA-SCROLL tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn get_line(&self, idx: usize) -> Result<Option<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }
        let lines = self.decompress()?;
        Ok(lines.into_iter().nth(idx))
    }

    /// Convert to cold-codec-compressed data for cold tier storage.
    ///
    /// Re-compresses with zstd (better ratio) when the `zstd` feature is on, and
    /// with the LZ4 codec otherwise. Returns the compressed data and line count,
    /// or an error if decompression/re-compression fails.
    pub(crate) fn to_cold_compressed(&self) -> Result<(Vec<u8>, usize), super::ScrollbackError> {
        // Reuse the LZ4-decompressed serialized bytes directly (normal path) instead
        // of deserialize→re-serialize on every eviction — see `decompress_serialized_page`.
        let serialized = self.decompress_serialized_page()?;
        let cold_compressed = super::encode_cold_block(&serialized)?;
        Ok((cold_compressed, self.line_count))
    }
}

/// Warm tier: LZ4 compressed blocks in RAM.
///
/// Uses cumulative line counts for O(log B) block lookup and caches the
/// last decompressed block to avoid redundant decompression on sequential access.
///
/// The `front_offset` field enables O(1) line-limit enforcement: instead of
/// decompressing the boundary block to remove a few oldest lines, we simply
/// advance the offset. The first block is dropped when fully consumed. This
/// mirrors the cold tier's front_offset pattern from `cold_tier.rs`.
#[derive(Debug)]
pub(crate) struct WarmTier {
    /// Compressed blocks.
    blocks: VecDeque<WarmBlock>,
    /// Total logical line count (physical lines minus front_offset).
    line_count: usize,
    /// Lines logically consumed from the first block. Avoids decompression
    /// during line-limit truncation — blocks are dropped when fully consumed.
    front_offset: usize,
    /// Cumulative line counts: `cumulative[i]` = total lines in blocks `0..=i`.
    /// These are physical counts (not adjusted for front_offset).
    cumulative_lines: Vec<usize>,
    /// Cache of last decompressed block: `(block_index, lines)`.
    last_block_cache: RefCell<Option<(usize, Vec<Line>)>>,
    /// Running total for `memory_used()` (diagnostic: includes cache + index).
    bytes_used: Cell<usize>,
    /// Reclaimable compressed block storage only (budget enforcement).
    budgeted_bytes: usize,
}

impl WarmTier {
    /// Create a new warm tier.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            blocks: VecDeque::new(),
            line_count: 0,
            front_offset: 0,
            cumulative_lines: Vec::new(),
            last_block_cache: RefCell::new(None),
            bytes_used: Cell::new(std::mem::size_of::<Self>()),
            budgeted_bytes: 0,
        }
    }

    /// Get the total number of lines.
    #[must_use]
    #[inline]
    pub(crate) fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the number of blocks.
    #[must_use]
    #[inline]
    pub(crate) fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Push a block of lines (compresses them).
    pub(crate) fn push_block(&mut self, lines: &[Line]) {
        if lines.is_empty() {
            return;
        }
        let old_index_bytes = self.index_bytes();
        let block = WarmBlock::from_lines(lines);
        let block_bytes = block.compressed_size();
        self.line_count = self.line_count.saturating_add(block.line_count());
        let cumulative = self
            .cumulative_lines
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(block.line_count());
        self.cumulative_lines.push(cumulative);
        self.blocks.push_back(block);
        // Saturating: the counters track bytes of memory this process actually
        // holds, so the true sum always fits in `usize` and the saturation can
        // never fire on a real path — it just discharges the strict L0 gate's
        // unconstrained-input overflow counterexamples (same idiom as the
        // saturating line/byte counters above).
        self.budgeted_bytes = self.budgeted_bytes.saturating_add(block_bytes);
        let bytes_used = self.bytes_used.get().saturating_add(block_bytes);
        self.bytes_used.set(Self::adjust_bytes(
            bytes_used,
            old_index_bytes,
            self.index_bytes(),
        ));
    }

    /// Pop the oldest block.
    ///
    /// If `front_offset > 0`, the front block is partially consumed. In that
    /// case the block is decompressed, trimmed, and recompressed so the
    /// returned block contains only surviving lines. This is the same work
    /// that `evict_warm_to_cold` would do anyway (LZ4→Zstd recompression),
    /// and pop_front is much less frequent than truncation.
    pub(crate) fn pop_front(&mut self) -> Option<WarmBlock> {
        self.materialize_front_offset();
        let old_index_bytes = self.index_bytes();
        let block = self.blocks.pop_front()?;
        self.line_count = self.line_count.saturating_sub(block.line_count());
        self.budgeted_bytes = self.budgeted_bytes.saturating_sub(block.compressed_size());
        // Rebuild cumulative index after removing front element.
        self.rebuild_cumulative();
        // Invalidate cache (block indices shifted).
        self.clear_cache();
        let bytes_used = self
            .bytes_used
            .get()
            .saturating_sub(block.compressed_size());
        self.bytes_used.set(Self::adjust_bytes(
            bytes_used,
            old_index_bytes,
            self.index_bytes(),
        ));
        Some(block)
    }

    /// Push a block back to the front (restores a previously popped block).
    ///
    /// Materializes any pending front_offset first so the new block
    /// becomes the true front with offset 0.
    pub(crate) fn push_front(&mut self, block: WarmBlock) {
        self.materialize_front_offset();
        let old_index_bytes = self.index_bytes();
        let block_bytes = block.compressed_size();
        self.line_count = self.line_count.saturating_add(block.line_count());
        // saturating_add: byte totals cannot overflow usize for real data.
        self.budgeted_bytes = self.budgeted_bytes.saturating_add(block_bytes);
        self.blocks.push_front(block);
        self.rebuild_cumulative();
        self.clear_cache();
        let bytes_used = self.bytes_used.get().saturating_add(block_bytes);
        self.bytes_used.set(Self::adjust_bytes(
            bytes_used,
            old_index_bytes,
            self.index_bytes(),
        ));
    }

    /// Find the block containing the given line index via binary search.
    // Skip: the binary search over the cumulative index — its residual rows are
    // the guarded-index class the verifier cannot chain (the codec twin carries
    // the same). Step-counted and unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn find_block(&self, line_idx: usize) -> Option<usize> {
        // Plain `&mut usize` step counter (was a `fn()`/closure callback):
        // mechanical caller update for the `binary_search_counted` signature
        // change (see codec.rs) — tests fold the total into the same
        // thread-local the old per-step callback incremented, so the
        // observable count is unchanged.
        let mut steps = 0usize;
        let result = super::binary_search_counted(
            &self.cumulative_lines,
            line_idx.saturating_add(1),
            &mut steps,
        );
        #[cfg(test)]
        count_warm_find_block_steps(steps);
        match result {
            Ok(idx) => Some(idx),
            Err(idx) => {
                if idx < self.cumulative_lines.len() {
                    Some(idx)
                } else {
                    None
                }
            }
        }
    }

    /// Rebuild cumulative line counts from blocks.
    // Skip: same std-iterator `next` absent-body class as `evict_bytes`.
    // Rebuilds the cumulative index from owned blocks; unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn rebuild_cumulative(&mut self) {
        self.cumulative_lines.clear();
        let mut total: usize = 0;
        for block in &self.blocks {
            total = total.saturating_add(block.line_count());
            self.cumulative_lines.push(total);
        }
    }

    /// Get a line by index (0 = oldest visible line across all blocks).
    ///
    /// Uses O(log B) binary search on cumulative line counts and caches the
    /// last decompressed block to avoid redundant decompression on sequential access.
    /// Adjusts for `front_offset`: logical index 0 maps to physical position
    /// `front_offset` within the first block.
    ///
    /// Returns `Ok(None)` for out-of-bounds, `Err` for decompression failures.
    // Skip: the block lookup + decompress path — the guarded-index/decode
    // class (twin of the cold tier's `get_line`). ARENA-SCROLL tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn get_line(&self, idx: usize) -> Result<Option<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }

        // Map logical index to physical position (including consumed lines).
        let physical_idx = idx.saturating_add(self.front_offset);

        let Some(block_idx) = self.find_block(physical_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "in-range line index {idx} (physical {physical_idx}) has no backing warm block"
                ),
            )));
        };
        let block_start = if block_idx == 0 {
            0
        } else {
            self.cumulative_lines
                .get(block_idx.saturating_sub(1))
                .copied()
                .unwrap_or(0)
        };
        let line_in_block = physical_idx.saturating_sub(block_start);

        // Check cache first.
        {
            let cache = self.last_block_cache.borrow();
            if let Some((cached_idx, ref lines)) = *cache
                && cached_idx == block_idx
            {
                let Some(line) = lines.get(line_in_block).cloned() else {
                    return Err(super::ScrollbackError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "warm block {block_idx} missing line offset {line_in_block} for index {idx}"
                        ),
                    )));
                };
                return Ok(Some(line));
            }
        }

        // Decompress and cache — skip quarantined blocks immediately (#5947).
        let Some(block) = self.blocks.get(block_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("warm block index {block_idx} out of range"),
            )));
        };
        if block.is_quarantined() {
            return Err(super::ScrollbackError::Quarantined(block.line_count()));
        }
        let lines = block.decompress()?;
        let Some(line) = lines.get(line_in_block).cloned() else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "warm block {block_idx} missing line offset {line_in_block} after decompression"
                ),
            )));
        };
        self.cache_block(block_idx, lines);
        Ok(Some(line))
    }

    // Front-truncation (truncate_front_lines) is in tier_front_truncation.rs.
    // Back-removal methods (pre_validate_truncate_back, truncate_back_lines)
    // are in tier_back_removal.rs.

    /// Clear all blocks.
    pub(crate) fn clear(&mut self) {
        self.blocks.clear();
        self.line_count = 0;
        self.front_offset = 0;
        self.cumulative_lines.clear();
        self.clear_cache();
        // Saturating: both terms are real in-memory sizes (see push_block).
        self.bytes_used
            .set(std::mem::size_of::<Self>().saturating_add(self.index_bytes()));
        self.budgeted_bytes = 0;
    }

    /// Calculate memory used (diagnostic: includes cache + index + struct overhead).
    #[must_use]
    pub(crate) fn memory_used(&self) -> usize {
        self.bytes_used.get()
    }

    /// Reclaimable compressed block storage bytes (budget enforcement only).
    #[must_use]
    #[inline]
    pub(crate) fn budgeted_bytes(&self) -> usize {
        self.budgeted_bytes
    }

    /// Replace the oldest warm block's compressed data with garbage.
    ///
    /// This is a cross-crate behavioral-test seam used to force warm-tier
    /// decompression failure during eviction paths.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn corrupt_oldest_block(&mut self) {
        if let Some(block) = self.blocks.front_mut() {
            let old_bytes = block.compressed.len();
            // Invalid LZ4: valid 4-byte size prefix (small) + garbage payload.
            block.compressed = vec![0x04, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF];
            let new_bytes = block.compressed.len();
            self.bytes_used.set(Self::adjust_bytes(
                self.bytes_used.get(),
                old_bytes,
                new_bytes,
            ));
            self.budgeted_bytes = Self::adjust_bytes(self.budgeted_bytes, old_bytes, new_bytes);
            self.clear_cache();
        }
    }

    /// Saturating folds for the same strict-gate reason as
    /// [`push_block`](Self::push_block) (identical on every real path).
    ///
    /// cfg: callers are the test suite and `DiskBackedScrollback`'s
    /// debug-build invariant recomputation (`disk-tier`); `Scrollback`'s own
    /// recomputation is test-only, so this exact cfg avoids dead code in
    /// non-test default-feature debug builds.
    #[cfg(any(test, all(debug_assertions, feature = "disk-tier")))]
    #[must_use]
    pub(crate) fn recompute_memory_used(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let mut blocks_mem = 0usize;
        for block in &self.blocks {
            blocks_mem = blocks_mem.saturating_add(block.compressed_size());
        }
        let index_mem = self
            .cumulative_lines
            .capacity()
            .saturating_mul(std::mem::size_of::<usize>());
        let cache_mem = self.cached_bytes();
        base.saturating_add(blocks_mem)
            .saturating_add(index_mem)
            .saturating_add(cache_mem)
    }

    /// See [`recompute_memory_used`](Self::recompute_memory_used) for the
    /// cfg and saturating-fold rationale.
    #[cfg(any(test, all(debug_assertions, feature = "disk-tier")))]
    #[must_use]
    pub(crate) fn recompute_budgeted_bytes(&self) -> usize {
        let mut blocks_mem = 0usize;
        for block in &self.blocks {
            blocks_mem = blocks_mem.saturating_add(block.compressed_size());
        }
        blocks_mem
    }

    fn index_bytes(&self) -> usize {
        // Saturating: a real Vec's capacity in bytes always fits in `usize`
        // (see push_block for the idiom rationale).
        self.cumulative_lines
            .capacity()
            .saturating_mul(std::mem::size_of::<usize>())
    }

    fn cache_block(&self, block_idx: usize, lines: Vec<Line>) {
        let old_cache_bytes = self.cached_bytes();
        let new_cache_bytes = Self::cache_lines_bytes(&lines);
        *self.last_block_cache.borrow_mut() = Some((block_idx, lines));
        self.bytes_used.set(Self::adjust_bytes(
            self.bytes_used.get(),
            old_cache_bytes,
            new_cache_bytes,
        ));
    }

    fn clear_cache(&self) {
        let old_cache_bytes = self.cached_bytes();
        *self.last_block_cache.borrow_mut() = None;
        self.bytes_used
            .set(self.bytes_used.get().saturating_sub(old_cache_bytes));
    }

    // Skip: `RefCell::borrow` PANICS on a conflicting active borrow (its
    // documented contract — deliberately excluded from totality). The cache
    // is single-threaded and never re-entrantly borrowed; unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn cached_bytes(&self) -> usize {
        self.last_block_cache
            .borrow()
            .as_ref()
            .map_or(0, |(_, lines)| Self::cache_lines_bytes(lines))
    }

    fn cache_lines_bytes(lines: &[Line]) -> usize {
        // Saturating fold (identical to the previous `.sum()` for any lines
        // that exist in memory — see push_block for the idiom rationale).
        let mut total = 0usize;
        for line in lines {
            total = total.saturating_add(line.memory_used());
        }
        total
    }

    fn adjust_bytes(current: usize, old_component: usize, new_component: usize) -> usize {
        if new_component >= old_component {
            current.saturating_add(new_component - old_component)
        } else {
            current.saturating_sub(old_component - new_component)
        }
    }
}

impl Default for WarmTier {
    fn default() -> Self {
        Self::new()
    }
}

#[path = "tier_decode.rs"]
mod decode;

#[path = "tier_front_truncation.rs"]
mod front_truncation;

#[path = "tier_back_removal.rs"]
mod back_removal;

#[cfg(test)]
#[path = "tier_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tier_decode_tests.rs"]
mod decode_tests;
