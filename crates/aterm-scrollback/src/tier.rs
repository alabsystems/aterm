// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Warm tier: LZ4 compressed blocks in RAM.

use super::line::{Line, serialize_lines, serialize_pages};
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

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace WarmTierOffset

        -- State of the WarmTier cumulative index:
        -- cum_start tracks dead prefix length, cumulative_base tracks dropped line total,
        -- and live_lines tracks the surviving logical line count.
        structure IndexState where
            cum_start : Nat
            cumulative_base : Nat
            live_blocks : Nat
            live_lines : Nat
        deriving Repr

        def emptyState : IndexState :=
            { cum_start := 0, cumulative_base := 0, live_blocks := 0, live_lines := 0 }

        def pushBlock (s : IndexState) (block_lines : Nat) : IndexState :=
            { cum_start := s.cum_start
            , cumulative_base := s.cumulative_base
            , live_blocks := s.live_blocks + 1
            , live_lines := s.live_lines + block_lines }

        def dropFrontBlock (s : IndexState) (front_lines : Nat) : IndexState :=
            { cum_start := s.cum_start + 1
            , cumulative_base := s.cumulative_base + front_lines
            , live_blocks := s.live_blocks - 1
            , live_lines := s.live_lines - front_lines }

        def compactDeadPrefix (s : IndexState) : IndexState :=
            { cum_start := 0
            , cumulative_base := s.cumulative_base
            , live_blocks := s.live_blocks
            , live_lines := s.live_lines }

        theorem empty_has_zero_offset :
            And (emptyState.cum_start = 0) (emptyState.cumulative_base = 0) := by
            exact And.intro rfl rfl

        theorem compact_preserves_live_lines (s : IndexState) :
            (compactDeadPrefix s).live_lines = s.live_lines := by
            rfl

        theorem compact_preserves_cumulative_base (s : IndexState) :
            (compactDeadPrefix s).cumulative_base = s.cumulative_base := by
            rfl

        theorem compact_resets_cum_start (s : IndexState) :
            (compactDeadPrefix s).cum_start = 0 := by
            rfl

        theorem push_then_drop_roundtrip_trace :
            And ((dropFrontBlock (pushBlock (pushBlock emptyState 5) 7) 5).live_lines = 7)
                (And ((dropFrontBlock (pushBlock (pushBlock emptyState 5) 7) 5).cumulative_base = 5)
                     ((dropFrontBlock (pushBlock (pushBlock emptyState 5) 7) 5).cum_start = 1)) := by
            exact And.intro rfl (And.intro rfl rfl)

        end WarmTierOffset
    }
}

/// Consecutive decompression failures before a warm block is quarantined.
pub(crate) const QUARANTINE_THRESHOLD: u8 = 3;

/// Decompressed-block cache slots.
const CACHE_SLOTS: usize = 2;

/// The decompressed-block cache itself: one `(block_index, lines)` pair per
/// slot, `None` while the slot is cold.
type BlockCache = RefCell<[Option<(usize, Vec<Line>)>; CACHE_SLOTS]>;

/// A compressed block of lines (LZ4).
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone)]
pub(crate) struct WarmBlock {
    /// LZ4 compressed data.
    compressed: Vec<u8>,
    /// Number of lines in block (for counting without decompression).
    line_count: usize,
    /// Consecutive decompression failure count. Reset on success.
    decompress_failures: Cell<u8>,
}

impl WarmBlock {
    /// Create a WarmBlock with corrupt compressed data (invalid LZ4).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn with_corrupt_data(line_count: usize) -> Self {
        Self {
            compressed: vec![0x04, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF],
            line_count,
            decompress_failures: std::cell::Cell::new(0),
        }
    }

    /// Create a warm block from lines.
    #[must_use]
    pub(crate) fn from_lines(lines: &[Line]) -> Self {
        Self::from_page(lines.len(), &serialize_lines(lines))
    }

    /// Create a warm block from a serialized page of `line_count` lines.
    #[must_use]
    fn from_page(line_count: usize, serialized: &[u8]) -> Self {
        let compressed = crate::lz4::compress_prepend_size(serialized).unwrap_or_default();

        Self {
            compressed,
            line_count,
            decompress_failures: Cell::new(0),
        }
    }

    /// Get the number of lines in this block.
    #[must_use]
    #[inline]
    pub(crate) const fn line_count(&self) -> usize {
        self.line_count
    }

    /// Returns true if this block has been quarantined due to repeated failures.
    #[must_use]
    #[inline]
    pub(crate) const fn is_quarantined(&self) -> bool {
        self.decompress_failures.get() >= QUARANTINE_THRESHOLD
    }

    /// Consecutive decompression failure count.
    #[cfg(test)]
    pub(crate) const fn failure_count(&self) -> u8 {
        self.decompress_failures.get()
    }

    /// Record a decompression failure.
    #[cfg(test)]
    pub(crate) fn record_failure(&self) -> bool {
        let failures = self.decompress_failures.get().saturating_add(1);
        self.decompress_failures.set(failures);
        failures == QUARANTINE_THRESHOLD
    }

    /// Get the compressed size in bytes.
    #[must_use]
    #[inline]
    pub(crate) const fn compressed_size(&self) -> usize {
        self.compressed.len()
    }

    /// Get a specific line by index within this block.
    #[cfg(test)]
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn get_line(&self, idx: usize) -> Result<Option<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }
        let lines = self.decompress()?;
        Ok(lines.into_iter().nth(idx))
    }

    /// Convert to cold-codec-compressed data for cold tier storage.
    pub(crate) fn to_cold_compressed(&self) -> Result<(Vec<u8>, usize), super::ScrollbackError> {
        let serialized = self.decompress_serialized_page()?;
        let cold_compressed = super::encode_cold_block(&serialized)?;
        Ok((cold_compressed, self.line_count))
    }
}

/// Warm tier: LZ4 compressed blocks in RAM.
#[derive(Debug)]
pub(crate) struct WarmTier {
    /// Compressed blocks.
    blocks: VecDeque<WarmBlock>,
    /// Total logical line count (physical lines minus front_offset).
    line_count: usize,
    /// Lines logically consumed from the first block. Avoids decompression
    /// during line-limit truncation — blocks are dropped when fully consumed.
    front_offset: usize,
    /// Cumulative line counts: for live block `i` (`0..blocks.len()`),
    /// `cumulative_lines[cum_start + i] - cumulative_base` = total physical
    /// lines in live blocks `0..=i` (not adjusted for `front_offset`).
    cumulative_lines: Vec<usize>,
    /// Dead-prefix length of `cumulative_lines` (entries of dropped blocks).
    cum_start: usize,
    /// Absolute cumulative value at the current front: total physical lines
    /// in all blocks dropped since the last compaction/clear/rebuild.
    cumulative_base: usize,
    /// Cache of recently decompressed blocks: `(block_index, lines)` per slot,
    /// filled round-robin.
    last_block_cache: BlockCache,
    /// Round-robin write cursor into `last_block_cache`.
    cache_next: Cell<usize>,
    /// Running total for `memory_used()` (diagnostic: includes cache + index).
    bytes_used: Cell<usize>,
    /// Reclaimable compressed block storage only (budget enforcement).
    budgeted_bytes: usize,
}

impl WarmTier {
    /// Create a new warm tier.
    #[must_use]
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            blocks: VecDeque::new(),
            line_count: 0,
            front_offset: 0,
            cumulative_lines: Vec::new(),
            cum_start: 0,
            cumulative_base: 0,
            last_block_cache: RefCell::new([const { None }; CACHE_SLOTS]),
            cache_next: Cell::new(0),
            bytes_used: Cell::new(std::mem::size_of::<Self>()),
            budgeted_bytes: 0,
        }
    }

    /// Get the total number of lines.
    #[must_use]
    #[inline]
    pub(crate) const fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the number of blocks.
    #[must_use]
    #[inline]
    pub(crate) fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Push a batch of lines (compresses them).
    pub(crate) fn push_block(&mut self, lines: &[Line]) {
        let pages = serialize_pages(lines, crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES);
        for (line_count, page) in pages {
            self.push_compressed(WarmBlock::from_page(line_count, &page));
        }
    }

    /// Append one compressed block at the newest end.
    fn push_compressed(&mut self, block: WarmBlock) {
        let old_index_bytes = self.index_bytes();
        let block_bytes = block.compressed_size();
        self.line_count = self.line_count.saturating_add(block.line_count());
        let cumulative = self
            .cumulative_lines
            .last()
            .copied()
            .unwrap_or(self.cumulative_base)
            .saturating_add(block.line_count());
        self.cumulative_lines.push(cumulative);
        self.blocks.push_back(block);
        self.budgeted_bytes = self.budgeted_bytes.saturating_add(block_bytes);
        let bytes_used = self.bytes_used.get().saturating_add(block_bytes);
        self.bytes_used.set(Self::adjust_bytes(
            bytes_used,
            old_index_bytes,
            self.index_bytes(),
        ));
    }

    /// Pop the oldest block.
    pub(crate) fn pop_front(&mut self) -> Option<WarmBlock> {
        self.materialize_front_offset();
        let old_index_bytes = self.index_bytes();
        let block = self.blocks.pop_front()?;
        self.line_count = self.line_count.saturating_sub(block.line_count());
        self.budgeted_bytes = self.budgeted_bytes.saturating_sub(block.compressed_size());
        self.drop_front_index_entries(1);
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

    /// Insert a corrupted warm block at the front (oldest position).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn push_front_corrupt(&mut self, line_count: usize) {
        let block = WarmBlock::with_corrupt_data(line_count);
        self.push_front(block);
    }

    /// Push a block back to the front (restores a previously popped block).
    pub(crate) fn push_front(&mut self, block: WarmBlock) {
        self.materialize_front_offset();
        let old_index_bytes = self.index_bytes();
        let block_bytes = block.compressed_size();
        self.line_count = self.line_count.saturating_add(block.line_count());
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

    /// Live (non-dropped) region of the cumulative index.
    #[inline]
    pub(super) fn live_cumulative(&self) -> &[usize] {
        self.cumulative_lines.get(self.cum_start..).unwrap_or(&[])
    }

    /// Drop the first `k` live cumulative entries in O(1) amortized.
    pub(super) fn drop_front_index_entries(&mut self, k: usize) {
        if k == 0 {
            return;
        }
        let live_len = self.live_cumulative().len();
        if k >= live_len {
            self.cumulative_lines.clear();
            self.cum_start = 0;
            self.cumulative_base = 0;
            return;
        }
        let last_dropped = self.cum_start.saturating_add(k).saturating_sub(1);
        self.cumulative_base = self
            .cumulative_lines
            .get(last_dropped)
            .copied()
            .unwrap_or(self.cumulative_base);
        self.cum_start = self.cum_start.saturating_add(k);
        if self.cum_start >= 64
            && self.cum_start > self.cumulative_lines.len().saturating_sub(self.cum_start)
        {
            let dead = self.cum_start;
            self.cumulative_lines.drain(..dead);
            self.cum_start = 0;
        }
    }

    /// Find the block containing the given line index via binary search.
    #[cfg_attr(trust_verify, trust::skip)]
    fn find_block(&self, line_idx: usize) -> Option<usize> {
        let mut steps = 0usize;
        let target = line_idx
            .saturating_add(self.cumulative_base)
            .saturating_add(1);
        let live = self.live_cumulative();
        let result = super::binary_search_counted(live, target, &mut steps);
        #[cfg(test)]
        count_warm_find_block_steps(steps);
        match result {
            Ok(idx) => Some(idx),
            Err(idx) => {
                if idx < live.len() {
                    Some(idx)
                } else {
                    None
                }
            }
        }
    }

    /// Rebuild cumulative line counts from blocks.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn rebuild_cumulative(&mut self) {
        self.cumulative_lines.clear();
        self.cum_start = 0;
        self.cumulative_base = 0;
        let mut total: usize = 0;
        for block in &self.blocks {
            total = total.saturating_add(block.line_count());
            self.cumulative_lines.push(total);
        }
    }

    /// Get a line by index (0 = oldest visible line across all blocks).
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn get_line(&self, idx: usize) -> Result<Option<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }

        let physical_idx = idx.saturating_add(self.front_offset);

        let Some((block_idx, line_in_block)) = self.locate(physical_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "in-range line index {idx} (physical {physical_idx}) has no backing warm block"
                ),
            )));
        };

        {
            let cache = self.last_block_cache.borrow();
            if let Some((_, lines)) = cache
                .iter()
                .flatten()
                .find(|(cached_idx, _)| *cached_idx == block_idx)
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

    /// Clear all blocks.
    pub(crate) fn clear(&mut self) {
        self.blocks.clear();
        self.line_count = 0;
        self.front_offset = 0;
        self.cumulative_lines.clear();
        self.cum_start = 0;
        self.cumulative_base = 0;
        self.clear_cache();
        self.bytes_used
            .set(std::mem::size_of::<Self>().saturating_add(self.index_bytes()));
        self.budgeted_bytes = 0;
    }

    /// Calculate memory used (diagnostic: includes cache + index + struct overhead).
    #[must_use]
    #[inline]
    pub(crate) const fn memory_used(&self) -> usize {
        self.bytes_used.get()
    }

    /// Reclaimable compressed block storage bytes (budget enforcement only).
    #[must_use]
    #[inline]
    pub(crate) const fn budgeted_bytes(&self) -> usize {
        self.budgeted_bytes
    }

    /// Test seam: replace the oldest warm block's compressed data with garbage to force decompression failure on eviction paths.
    #[cfg(test)]
    pub(crate) fn corrupt_oldest_block(&mut self) {
        if let Some(block) = self.blocks.front_mut() {
            let old_bytes = block.compressed.len();
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

    /// Saturating folds for the Trust lane, as in [`push_compressed`](Self::push_compressed).
    #[cfg(test)]
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

    /// See [`recompute_memory_used`](Self::recompute_memory_used) for the cfg and saturating-fold rationale.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn recompute_budgeted_bytes(&self) -> usize {
        let mut blocks_mem = 0usize;
        for block in &self.blocks {
            blocks_mem = blocks_mem.saturating_add(block.compressed_size());
        }
        blocks_mem
    }

    #[inline]
    const fn index_bytes(&self) -> usize {
        self.cumulative_lines
            .capacity()
            .saturating_mul(std::mem::size_of::<usize>())
    }

    fn cache_block(&self, block_idx: usize, lines: Vec<Line>) {
        let old_cache_bytes = self.cached_bytes();
        {
            let mut cache = self.last_block_cache.borrow_mut();
            let slot = self.cache_next.get() % CACHE_SLOTS;
            if let Some(entry) = cache.get_mut(slot) {
                *entry = Some((block_idx, lines));
            }
            self.cache_next.set(slot.saturating_add(1) % CACHE_SLOTS);
        }
        let new_cache_bytes = self.cached_bytes();
        self.bytes_used.set(Self::adjust_bytes(
            self.bytes_used.get(),
            old_cache_bytes,
            new_cache_bytes,
        ));
    }

    /// Drop every cached block.
    fn clear_cache(&self) {
        let old_cache_bytes = self.cached_bytes();
        *self.last_block_cache.borrow_mut() = [const { None }; CACHE_SLOTS];
        self.cache_next.set(0);
        self.bytes_used
            .set(self.bytes_used.get().saturating_sub(old_cache_bytes));
    }

    #[cfg_attr(trust_verify, trust::skip)]
    fn cached_bytes(&self) -> usize {
        let cache = self.last_block_cache.borrow();
        let mut total = 0usize;
        for (_, lines) in cache.iter().flatten() {
            total = total.saturating_add(Self::cache_lines_bytes(lines));
        }
        total
    }

    fn cache_lines_bytes(lines: &[Line]) -> usize {
        let mut total = 0usize;
        for line in lines {
            total = total.saturating_add(line.memory_used());
        }
        total
    }

    #[inline]
    const fn adjust_bytes(current: usize, old_component: usize, new_component: usize) -> usize {
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
