// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Warm tier: LZ4 compressed blocks in RAM.
//!
//! Hot tier is in [`hot_tier`](super::hot_tier).
//! Cold tier is in [`cold_tier`](super::cold_tier).

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
/// 3 failures = conclusive evidence of persistent corruption.
pub(crate) const QUARANTINE_THRESHOLD: u8 = 3;

/// Decompressed-block cache slots.
///
/// A render frame walks the visible rows top-to-bottom (ascending scrollback
/// indices), so with one slot a viewport straddling a block boundary decodes
/// both blocks every frame. A viewport spans at most two blocks
/// (`DEFAULT_BLOCK_SIZE` is 100 lines), so two slots turn that into hits.
const CACHE_SLOTS: usize = 2;

/// The decompressed-block cache itself: one `(block_index, lines)` pair per
/// slot, `None` while the slot is cold. Named because the inline spelling trips
/// `clippy::type_complexity`, which this crate denies via `clippy::all`.
type BlockCache = RefCell<[Option<(usize, Vec<Line>)>; CACHE_SLOTS]>;

/// A compressed block of lines (LZ4).
// Skip (propagates to the derived impls: the checker consults the impl subject
// for macro-generated items): the derived Clone's field clones are the
// idiomatic-alloc class, as `Line::clone`.
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
    /// Create a WarmBlock with corrupt compressed data (invalid LZ4).
    ///
    /// The block reports `line_count` lines but its compressed data cannot be
    /// decompressed. Used to test error paths in cold tier push and warm-to-cold
    /// eviction where re-compression fails.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn with_corrupt_data(line_count: usize) -> Self {
        Self {
            // Invalid LZ4: valid 4-byte size prefix (small) + garbage payload
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
        // `compress_prepend_size` fails only past u32::MAX bytes, which a warm
        // page never reaches; the empty-buffer fallback discharges the Trust L0
        // panic-freedom obligation.
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
    /// Production code goes through `decompress()`, which tracks failures; this
    /// forces a quarantine in tests.
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
    // Skip: the block lookup + decompress path, the same guarded-index/decode
    // class as the cold tier's `get_line`. Round-trip tested.
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
    /// Decodes the LZ4 block and re-encodes it with the cold codec. Returns the
    /// compressed data and line count, or an error if decompression or
    /// re-compression fails.
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
/// O(log B) block lookup over cumulative line counts, with recently
/// decompressed blocks cached for sequential access. `front_offset` makes
/// line-limit enforcement O(1): the oldest lines are consumed by advancing it,
/// without decompressing, and the first block drops once fully consumed (as in
/// the cold tier).
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
    ///
    /// Stored with a dead-prefix index (`cum_start`) and base offset
    /// (`cumulative_base`) so front-eviction (`pop_front`,
    /// `truncate_front_lines`) is O(1) amortized instead of O(B) per pop.
    cumulative_lines: Vec<usize>,
    /// Dead-prefix length of `cumulative_lines` (entries of dropped blocks).
    cum_start: usize,
    /// Absolute cumulative value at the current front: total physical lines
    /// in all blocks dropped since the last compaction/clear/rebuild.
    cumulative_base: usize,
    /// Cache of recently decompressed blocks: `(block_index, lines)` per slot,
    /// filled round-robin. See [`CACHE_SLOTS`] for why there is more than one.
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
    pub(crate) fn new() -> Self {
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
    pub(crate) fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the number of blocks.
    #[must_use]
    #[inline]
    pub(crate) fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Push a batch of lines (compresses them).
    ///
    /// Bounded by bytes as well as lines: the batch becomes as many
    /// consecutive blocks as keep each block's serialized page within
    /// [`MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES`](crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES),
    /// the most a reader decodes. Rows whose every cell carries its own 8 KiB
    /// link exceed it, and a block its reader refused would be unreadable until
    /// eviction quarantined it. Every line an honest grid makes fits a page
    /// alone (`line_codec_block` asserts it).
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
        // Saturating: the counters track bytes this process holds, so the true
        // sum fits; saturation only discharges the strict L0 gate's
        // unconstrained-input overflow counterexamples.
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
    /// A partially consumed front block (`front_offset > 0`) is decompressed,
    /// trimmed and recompressed first, so the returned block holds only
    /// surviving lines; eviction recompresses it for cold anyway.
    pub(crate) fn pop_front(&mut self) -> Option<WarmBlock> {
        self.materialize_front_offset();
        let old_index_bytes = self.index_bytes();
        let block = self.blocks.pop_front()?;
        self.line_count = self.line_count.saturating_sub(block.line_count());
        self.budgeted_bytes = self.budgeted_bytes.saturating_sub(block.compressed_size());
        // Advance cumulative index front in O(1) amortized instead of O(B) rebuild.
        self.drop_front_index_entries(1);
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

    /// Insert a corrupted warm block at the front (oldest position): a corrupt
    /// block that fails eviction to cold, for quarantine tests and, in a debug
    /// build, downstream readers that must survive one.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn push_front_corrupt(&mut self, line_count: usize) {
        let block = WarmBlock::with_corrupt_data(line_count);
        self.push_front(block);
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

    /// Live (non-dropped) region of the cumulative index. Total `get` keeps
    /// this obligation-free even if `cum_start` were out of sync.
    #[inline]
    pub(super) fn live_cumulative(&self) -> &[usize] {
        self.cumulative_lines.get(self.cum_start..).unwrap_or(&[])
    }

    /// Drop the first `k` live cumulative entries in O(1) amortized: advance
    /// `cum_start` and `cumulative_base`, leaving every surviving value
    /// untouched (`surviving - new_base == old_surviving - old_base`).
    ///
    /// When the dead prefix grows past the live suffix (with a 64-entry floor),
    /// compact in one `drain(..cum_start)` pass.
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
        // `1 <= k < live_len`, so `cum_start + k - 1 < cumulative_lines.len()`.
        let last_dropped = self.cum_start.saturating_add(k).saturating_sub(1);
        self.cumulative_base = self
            .cumulative_lines
            .get(last_dropped)
            .copied()
            .unwrap_or(self.cumulative_base);
        self.cum_start = self.cum_start.saturating_add(k);
        // Amortized compaction once dead > live (and at least 64 entries): one
        // O(dead) move. `cumulative_base` stays the baseline for the compacted
        // slice, so surviving entries are not rewritten.
        if self.cum_start >= 64
            && self.cum_start > self.cumulative_lines.len().saturating_sub(self.cum_start)
        {
            let dead = self.cum_start;
            self.cumulative_lines.drain(..dead);
            self.cum_start = 0;
        }
    }

    /// Find the block containing the given line index via binary search.
    // Skip: the binary search over the cumulative index — its residual rows are
    // the guarded-index class the verifier cannot chain (the codec twin carries
    // the same). Step-counted and unit-tested.
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
    // Skip: same std-iterator `next` absent-body class as the cold tier's
    // `evict_bytes`. Rebuilds the cumulative index from owned blocks; unit-tested.
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
    ///
    /// O(log B) lookup on the cumulative counts, with decompressed blocks cached
    /// for sequential access; logical index 0 is physical position
    /// `front_offset` in the first block. `Ok(None)` out of bounds, `Err` on
    /// decompression failure.
    // Skip: the block lookup + decompress path, the guarded-index/decode class
    // (twin of the cold tier's `get_line`).
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn get_line(&self, idx: usize) -> Result<Option<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }

        // Map logical index to physical position (including consumed lines).
        let physical_idx = idx.saturating_add(self.front_offset);

        let Some((block_idx, line_in_block)) = self.locate(physical_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "in-range line index {idx} (physical {physical_idx}) has no backing warm block"
                ),
            )));
        };

        // Check every cache slot first.
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

        // Decompress and cache; a quarantined block fails fast.
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
    // Back-removal (truncate_back_lines) is in tier_back_removal.rs.

    /// Clear all blocks.
    pub(crate) fn clear(&mut self) {
        self.blocks.clear();
        self.line_count = 0;
        self.front_offset = 0;
        self.cumulative_lines.clear();
        self.cum_start = 0;
        self.cumulative_base = 0;
        self.clear_cache();
        // Saturating: both terms are real in-memory sizes (see push_compressed).
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

    /// Test seam: replace the oldest warm block's compressed data with garbage
    /// to force decompression failure on eviction paths.
    #[cfg(test)]
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

    /// Saturating folds for the strict gate, as in
    /// [`push_compressed`](Self::push_compressed).
    ///
    /// Test-only: the invariant recomputation that calls it is.
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

    /// See [`recompute_memory_used`](Self::recompute_memory_used) for the
    /// cfg and saturating-fold rationale.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn recompute_budgeted_bytes(&self) -> usize {
        let mut blocks_mem = 0usize;
        for block in &self.blocks {
            blocks_mem = blocks_mem.saturating_add(block.compressed_size());
        }
        blocks_mem
    }

    fn index_bytes(&self) -> usize {
        // Saturating: a real Vec's capacity in bytes fits (see push_compressed).
        self.cumulative_lines
            .capacity()
            .saturating_mul(std::mem::size_of::<usize>())
    }

    fn cache_block(&self, block_idx: usize, lines: Vec<Line>) {
        // Total before → total after: filling a slot also drops what it held, so
        // the delta is not the new entry's size. `bytes_used` must equal
        // `recompute_memory_used()` or the drift assert in
        // `scrollback_accounting.rs` fires. Both `cached_bytes()` calls sit
        // outside the `borrow_mut` scope, which borrows the same `RefCell`.
        let old_cache_bytes = self.cached_bytes();
        {
            let mut cache = self.last_block_cache.borrow_mut();
            let slot = self.cache_next.get() % CACHE_SLOTS;
            // `slot < CACHE_SLOTS`, so the `else` is unreachable; a missed fill
            // would only cost a later decompression.
            if let Some(entry) = cache.get_mut(slot) {
                *entry = Some((block_idx, lines));
            }
            // saturating_add cannot overflow (`slot < CACHE_SLOTS`); the modulo
            // keeps the cursor in range.
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
    ///
    /// Clear-all is an invariant: `pop_front`/`push_front` renumber every block,
    /// so a surviving entry keyed by its old index would serve the wrong lines.
    /// Never make this selective.
    fn clear_cache(&self) {
        let old_cache_bytes = self.cached_bytes();
        *self.last_block_cache.borrow_mut() = [const { None }; CACHE_SLOTS];
        self.cache_next.set(0);
        self.bytes_used
            .set(self.bytes_used.get().saturating_sub(old_cache_bytes));
    }

    // Skip: `RefCell::borrow` panics on a conflicting active borrow (its
    // documented contract — deliberately excluded from totality). The cache
    // is single-threaded and never re-entrantly borrowed; unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn cached_bytes(&self) -> usize {
        // Saturating fold over every slot (see push_compressed).
        let cache = self.last_block_cache.borrow();
        let mut total = 0usize;
        for (_, lines) in cache.iter().flatten() {
            total = total.saturating_add(Self::cache_lines_bytes(lines));
        }
        total
    }

    fn cache_lines_bytes(lines: &[Line]) -> usize {
        // Saturating fold (see push_compressed).
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
